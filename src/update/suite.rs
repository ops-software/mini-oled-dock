//! The suite's update path: one manifest, several exes (U1).
//!
//! U1c: one component other than the dock is swapped into the install folder so that its name always resolves, every
//! change recorded in update.jnl (FileSwap entries, flushed) before it is made; its run is committed after the
//! component proved itself, reverted with F-P5's ownership rule when it did not, and replayed (uncommitted runs only)
//! at the start of every sync. U1d: the proof, a selftest at medium (F-P7) or a parse of the language file.
//! U1e: `--suite-sync`, an elevated child the dock starts once per start and after every successful check, brings every
//! component to the dock's own release from their previous copies, the stage or the release; the dock keeps its
//! result line for the Suite page (`status`), and its stage of a newer release carries the changed components.

use std::path::{Path, PathBuf};
use std::time::Duration;

use oled_sys::journal_os::FileSink;
use oled_trust::journal::{
    self, Change, Digest, Hive, Journal, JournalOs, Kind, OsErr, RegData, RegPath, RegSnapshot, RunId, Sink, TaskName,
    UndoReport, UndoSet,
};
use oled_trust::manifest::release::{Asset, Release, Version};
use oled_trust::manifest::suite::{
    encode_manifest, parse_manifest, plan_convergence, Component, ComponentId, OnDisk, Source, Stage, Step,
    SuiteManifest, COMPONENT_MAX_BYTES, SUITE_ASSET, SUITE_MAX_BYTES,
};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::ERROR_UNABLE_TO_MOVE_REPLACEMENT_2;
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, ReplaceFileW, FILE_FLAG_OPEN_REPARSE_POINT, MOVEFILE_DELAY_UNTIL_REBOOT, MOVEFILE_WRITE_THROUGH,
    REPLACEFILE_IGNORE_MERGE_ERRORS,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

use crate::shell_thread::Verb;

/// How a component's exe is put in place while an older copy of it may be running (U1s measured both forms of
/// `ReplaceFileW` and the rename pair over a running image: tests below).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Swap {
    /// `ReplaceFileW(X, X.new, X.old-<seq>)` (SPEC-platform 5.6.3): one call, and the name always resolves.
    Replace,
    /// `X -> X.old-<seq>`, then `X.new -> X`: the name is missing only between the two renames (update.jnl's replay
    /// covers a crash there).
    #[allow(dead_code)] // the fallback U1s measured; SWAP picks Replace
    RenamePair,
}

/// The swap U1c uses. `Replace` only while `ReplaceFileW` with a backup name succeeds over a running image on the CI
/// runner and on a real PC: `replace_with_backup_over_a_running_image` fails when it does not. The call without a backup
/// name is never used (over a running image it returns Ok but leaves the old image under an `X~RF<hex>.TMP` name that
/// no removal rule covers).
pub(crate) const SWAP: Swap = Swap::Replace;

#[allow(dead_code)] // the Suite page (P3-dock-suite) reads it
pub struct SuiteStatus {
    pub version: String,
    pub components: Vec<ComponentStatus>,
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
}

#[allow(dead_code)] // the Suite page (P3-dock-suite) reads it
pub struct ComponentStatus {
    pub id: ComponentId,
    pub installed: bool,
    pub version: Option<String>,
    pub stage: Option<Stage>,
    pub enabled: bool,
}

// ---------------------------------------------------------------------------------------------- U1c: the swap

/// How long a sync waits for update.jnl's lock.
pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(5);
/// The most `.old-<n>` names tried for one file.
const OLD_MAX: u32 = 999;
/// Win32 codes of the journal errors this OS reports.
const CODE_ACCESS_DENIED: u32 = 5;
const CODE_IO: u32 = 1117;

/// What a swap needs besides the folder; tests stand in for both.
pub(crate) trait SwapEnv {
    /// Called after each step of `swap_in` (1 to 6); false stops the swap there, as a crash would (tests).
    fn go_on(&mut self, _step: u8) -> bool {
        true
    }
    /// Asks Windows to delete `path` at the next restart: a replaced file that is a running image. Elevated only.
    fn delete_at_restart(&mut self, path: &Path) -> Result<(), String>;
}

/// The machine: no stop; the delete at restart is MoveFileExW's (PendingFileRenameOperations).
pub(crate) struct RealEnv;

impl SwapEnv for RealEnv {
    fn delete_at_restart(&mut self, path: &Path) -> Result<(), String> {
        let p = HSTRING::from(path.as_os_str());
        // SAFETY: `p` is NUL-terminated and lives for the call; a null new name means "delete".
        unsafe { MoveFileExW(&p, PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT) }
            .map_err(|e| format!("{}: deleting it at restart: {e}", name_of(path)))
    }
}

/// A component swapped in and not yet committed: its run in update.jnl, and the earlier previous copy kept aside
/// (as `<prev>.old-<n>`) until `commit_swap` deletes it or `revert` puts it back.
#[derive(Debug)]
pub(crate) struct Swapped {
    pub run: RunId,
    #[allow(dead_code)] // the tests and Debug read it
    pub id: ComponentId,
    pub prev_kept: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum SwapError {
    /// Never for dock.exe: the guard owns it.
    Dock,
    /// The staged file is not the manifest's (size or SHA-256) or cannot be read: nothing was recorded or touched.
    Staged(String),
    /// A step failed. `run` holds what was recorded (None: nothing was): the caller reverts it.
    Failed { run: Option<RunId>, why: String },
    /// Stopped after `step` by the environment (tests: a crash there).
    Stopped { run: Option<RunId>, step: u8 },
}

/// The install-relative names a swap (and its revert) makes for a component: its file, its previous copy and their
/// `.new` staging names (`ComponentId::install_names`); each of them also gets `.old-<n>` copies.
#[allow(dead_code)] // swap_names_are_removed holds FC1's removal list to it
pub(crate) fn swap_names(id: ComponentId) -> Vec<PathBuf> {
    id.install_names().into_iter().map(PathBuf::from).collect()
}

/// True for `<dir>\<name>` where name is a swap name of a component other than the dock, or one of its
/// `.old-<digits>` copies, compared without regard to case as Windows compares names. The only paths update.jnl's
/// replay ever touches: no record, however it got into the file, reaches dock.exe or anything else.
pub(crate) fn is_swap_path(dir: &Path, path: &Path) -> bool {
    let (Some(d), Some(p)) = (dir.to_str(), path.to_str()) else { return false };
    let (d, p) = (d.trim_end_matches('\\').to_lowercase(), p.to_lowercase());
    let Some(rel) = p.strip_prefix(d.as_str()).and_then(|r| r.strip_prefix('\\')) else { return false };
    ComponentId::ALL.into_iter().filter(|c| *c != ComponentId::Dock).flat_map(|c| c.install_names()).any(|name| {
        let name = name.to_lowercase();
        rel == name
            || rel
                .strip_prefix(name.as_str())
                .and_then(|r| r.strip_prefix(".old-"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Puts the staged copy of `comp` in place in the install folder `dir` (U1c, steps 1 to 6; the caller commits after
/// the selftest, step 7):
/// 1. the staged file's size and SHA-256 are the manifest's;
/// 2. a run begins and its FileSwap entries are recorded and flushed: first the previous copy's (when the current
///    file is to become the previous copy), then the file's;
/// 3. the staged file is copied to `<file>.new`, flushed and hashed again;
/// 4. when the file exists, it is copied to `<prev>.new`, flushed and checked, then put at `<prev>` (an earlier
///    previous copy is kept aside as `<prev>.old-<n>` until the commit, so a revert can put it back);
/// 5. the swap by the rule U1s measured (`SWAP`; a new file is renamed into place), so the name always resolves;
///    the replaced file is deleted, or deleted at the next restart when it is a running image;
/// 6. the file in place is hashed again; bytes that are not the release's (another program wrote `<file>.new` before
///    step 5 put it in place) are taken back at once: the file they replaced is put back from the previous copy,
///    which holds it, or the addition is deleted, so the revert finds the folder as it was.
///
/// A step that fails returns its run: the caller reverts it. Names come only from `ComponentId::install_names` and
/// the `.old-<n>` rule. Never for the dock.
pub(crate) fn swap_in(
    dir: &Path,
    comp: &Component,
    staged: &Path,
    j: &mut Journal,
    sink: &mut dyn Sink,
    env: &mut dyn SwapEnv,
) -> Result<Swapped, SwapError> {
    debug_assert_ne!(comp.id, ComponentId::Dock, "the guard owns dock.exe");
    if comp.id == ComponentId::Dock {
        return Err(SwapError::Dock);
    }
    let (dest, prev) = (dir.join(comp.id.file()), dir.join(comp.id.prev()));
    let (dest_new, prev_new) = (with_suffix(&dest, ".new"), with_suffix(&prev, ".new"));
    // 1.
    match hash_file(staged) {
        Ok(Some((h, n))) if h == comp.sha256 && n == comp.size => {}
        Ok(Some(_)) => return Err(SwapError::Staged(format!("{}: not the release's file", comp.id.id()))),
        Ok(None) => return Err(SwapError::Staged(format!("{}: nothing staged", comp.id.id()))),
        Err(why) => return Err(SwapError::Staged(why)),
    }
    step_done(env, None, 1)?;
    let unrecorded = |why: String| SwapError::Failed { run: None, why };
    if let Some(parent) = dest.parent().filter(|p| *p != dir) {
        real_dir(parent).map_err(unrecorded)?; // lang\: created with the install folder's inherited ACL
    }
    let current = hash_file(&dest).map_err(unrecorded)?.map(|(h, _)| h);
    let old_prev = match current {
        Some(_) => hash_file(&prev).map_err(unrecorded)?.map(|(h, _)| h),
        None => None,
    };
    // The previous copy changes only when the current file moves into it and it does not hold that file already.
    let new_prev = current.filter(|c| old_prev != Some(*c));
    let prev_kept = match (new_prev, old_prev) {
        (Some(_), Some(_)) => {
            Some(old_name(&prev).ok_or_else(|| unrecorded(format!("{}: no free .old name", comp.id.id())))?)
        }
        _ => None,
    };
    // 2.
    let run = j.begin_run();
    let failed = |why: String| SwapError::Failed { run: Some(run), why };
    let now = now_unix();
    if let Some(c) = new_prev {
        let kept = prev_kept.as_deref().map(path_text).transpose().map_err(failed)?;
        let change = Change::FileSwap { dest: path_text(&prev).map_err(failed)?, prev: kept, staged_sha256: c };
        journal::record(j, sink, run, Kind::Update, change, now).map_err(|e| failed(e.what))?;
    }
    let back = match current {
        Some(_) => Some(path_text(&prev).map_err(failed)?),
        None => None,
    };
    let change = Change::FileSwap { dest: path_text(&dest).map_err(failed)?, prev: back, staged_sha256: comp.sha256 };
    journal::record(j, sink, run, Kind::Update, change, now).map_err(|e| failed(e.what))?;
    step_done(env, Some(run), 2)?;
    // 3.
    copy_flushed(staged, &dest_new).map_err(failed)?;
    check_hash(&dest_new, comp.sha256).map_err(failed)?;
    step_done(env, Some(run), 3)?;
    // 4.
    if let Some(c) = new_prev {
        copy_flushed(&dest, &prev_new).map_err(failed)?;
        check_hash(&prev_new, c).map_err(failed)?;
        match &prev_kept {
            Some(kept) => replace_keeping(&prev, &prev_new, kept).map_err(failed)?,
            None => rename(&prev_new, &prev).map_err(failed)?,
        }
    }
    step_done(env, Some(run), 4)?;
    // 5.
    put_in_place(&dest, &dest_new, env).map_err(failed)?;
    step_done(env, Some(run), 5)?;
    // 6.
    if let Err(why) = check_hash(&dest, comp.sha256) {
        let back = match current {
            Some(c) => put_back(&dest, &prev, c, env),
            None => remove_stale(&dest),
        };
        return Err(failed(match back {
            Ok(()) => why,
            Err(e) => format!("{why}; {e}"),
        }));
    }
    step_done(env, Some(run), 6)?;
    Ok(Swapped { run, id: comp.id, prev_kept })
}

/// Step 7, once the component proved itself (U1d): its run is committed in update.jnl, so nothing ever reverts it,
/// then the earlier previous copy kept aside is deleted.
pub(crate) fn commit_swap(
    j: &mut Journal,
    sink: &mut dyn Sink,
    s: Swapped,
    env: &mut dyn SwapEnv,
) -> Result<(), OsErr> {
    journal::commit(j, sink, s.run)?;
    if let Some(kept) = &s.prev_kept {
        discard(kept, env);
    }
    Ok(())
}

/// Puts back what an uncommitted run of `id` changed (its selftest failed, or the swap stopped part-way): F-P5's
/// FileSwap rule per entry, newest first (the file still the staged one: the previous file back, or the addition
/// deleted; else it is no longer ours and is left, reported changed_by_other), each closed with Undone; a previous
/// copy stays while it is the only copy of the file the run replaced (`undo_run`). A committed run has no open entry
/// and is never touched. Once the run is closed, the swap's leftovers go.
pub(crate) fn revert(
    dir: &Path,
    id: ComponentId,
    run: RunId,
    j: &mut Journal,
    sink: &mut dyn Sink,
    env: &mut dyn SwapEnv,
) -> UndoReport {
    let report = undo_run(dir, run, j, sink, env);
    if !j.open_entries().iter().any(|e| e.run == run) {
        sweep(dir, &[id]);
    }
    report
}

/// What a replay of update.jnl did.
#[derive(Debug, Default)]
#[allow(dead_code)] // --recover (J1d) reports it
pub(crate) struct Replay {
    /// Uncommitted runs found.
    pub runs: u32,
    pub report: UndoReport,
    /// Nothing was left open, so update.jnl was deleted.
    pub deleted: bool,
}

/// The start of every sync: every run update.jnl holds that was never committed (a crash, a kill or a power loss
/// part-way through a swap, or between a selftest and its commit) is reverted, newest first, with the ownership rule;
/// committed runs are never touched. Then, when nothing is left open, the swaps' leftovers go. Idempotent.
pub(crate) fn replay_open_in(
    dir: &Path,
    j: &mut Journal,
    sink: &mut dyn Sink,
    env: &mut dyn SwapEnv,
) -> (u32, UndoReport) {
    let runs = newest_first(j.open_entries().into_iter().map(|e| e.run));
    let mut total = UndoReport::default();
    for run in &runs {
        let r = undo_run(dir, *run, j, sink, env);
        merge(&mut total, r);
    }
    if j.is_settled() {
        let others: Vec<ComponentId> = ComponentId::ALL.into_iter().filter(|c| *c != ComponentId::Dock).collect();
        sweep(dir, &others);
    }
    (u32::try_from(runs.len()).unwrap_or(u32::MAX), total)
}

/// Each run once, newest (highest id) first. Kept in that order as the runs come, by insertion: a stable sort of the
/// runs would link a sort routine of its own (about 3 KiB) into the release exe for a list of a few entries.
fn newest_first(runs: impl IntoIterator<Item = RunId>) -> Vec<RunId> {
    let mut out: Vec<RunId> = Vec::new();
    for run in runs {
        // `out` is in descending order: Err(i) is where `run` keeps it so; Ok means the run is already listed.
        if let Err(i) = out.binary_search_by(|r| run.0.cmp(&r.0)) {
            out.insert(i, run);
        }
    }
    out
}

/// `replay_open_in` under update.jnl's lock at `journal_path`, which is deleted when nothing is left open.
#[allow(dead_code)] // --recover (J1d) replays update.jnl through it; the sync keeps update.jnl open (sync_in)
pub(crate) fn replay_open(dir: &Path, journal_path: &Path, env: &mut dyn SwapEnv) -> Result<Replay, OsErr> {
    let (mut j, mut sink) = oled_sys::journal_os::open_locked_at(journal_path, journal::Scope::Machine, LOCK_WAIT)?;
    let (runs, report) = replay_open_in(dir, &mut j, &mut sink, env);
    let deleted = sink.delete_if_settled(&j)?;
    Ok(Replay { runs, report, deleted })
}

/// One uncommitted run undone by F-P5's rule: journal::undo, newest first, so a component's file entry (recorded after
/// its previous copy's) is undone before the previous copy's. A previous copy holds the only copy of the file the run
/// replaced until that file is back at its name, so FolderOs refuses to delete or replace it while the name holds
/// other bytes or cannot be read (its entry stays open and a later undo tries again). A missing name (an AV
/// quarantine, or a swap stopped between its renames) is no other program's file: when the previous copy still holds
/// the run's copy of it, that copy is put back there first, so both entries close and the folder is as it was.
fn undo_run(dir: &Path, run: RunId, j: &mut Journal, sink: &mut dyn Sink, env: &mut dyn SwapEnv) -> UndoReport {
    let copies: Vec<(PathBuf, Digest)> = j
        .open_entries()
        .into_iter()
        .filter(|e| e.run == run)
        .filter_map(|e| match &e.change {
            Change::FileSwap { dest, staged_sha256, .. } => Some((PathBuf::from(dest), *staged_sha256)),
            _ => None,
        })
        .collect();
    for (prev, kept) in copies {
        let Some(file) = file_of_prev(dir, &prev) else { continue };
        if matches!(hash_file(&file), Ok(None)) {
            let _ = put_back(&file, &prev, kept, env); // when it fails, the rule below leaves the previous copy
        }
    }
    journal::undo(j, sink, &mut FolderOs { dir, env: &mut *env }, &UndoSet::Run(run))
}

/// The component file whose previous copy is `path` (`<dir>\<prev>` of a component other than the dock, compared
/// without regard to case as `is_swap_path` compares), or None.
fn file_of_prev(dir: &Path, path: &Path) -> Option<PathBuf> {
    let (d, p) = (dir.to_str()?.trim_end_matches('\\').to_lowercase(), path.to_str()?.to_lowercase());
    let rel = p.strip_prefix(d.as_str())?.strip_prefix('\\')?;
    ComponentId::ALL
        .into_iter()
        .filter(|c| *c != ComponentId::Dock)
        .find(|c| c.prev().to_lowercase() == rel)
        .map(|c| dir.join(c.file()))
}

fn merge(total: &mut UndoReport, r: UndoReport) {
    total.restored += r.restored;
    total.already += r.already;
    total.unknown += r.unknown;
    total.changed_by_other.extend(r.changed_by_other);
    total.failed.extend(r.failed);
}

fn step_done(env: &mut dyn SwapEnv, run: Option<RunId>, step: u8) -> Result<(), SwapError> {
    if env.go_on(step) {
        Ok(())
    } else {
        Err(SwapError::Stopped { run, step })
    }
}

/// The install folder as the journal's OS for update.jnl: file changes only, and only on `is_swap_path` names (so
/// never on dock.exe). Registry and task changes are refused: update.jnl never holds them.
struct FolderOs<'a> {
    dir: &'a Path,
    env: &'a mut dyn SwapEnv,
}

impl FolderOs<'_> {
    fn ours(&self, path: &str) -> Result<PathBuf, OsErr> {
        let p = PathBuf::from(path);
        if is_swap_path(self.dir, &p) {
            Ok(p)
        } else {
            Err(os_err(CODE_ACCESS_DENIED, "update journal: not a component file of this install"))
        }
    }

    /// `path` may be deleted or replaced, unless it is a component's previous copy and the component's file does not
    /// hold the same bytes or cannot be read: then it holds the only copy of the file its run replaced (`undo_run`).
    fn not_the_only_copy(&self, path: &Path) -> Result<(), OsErr> {
        let Some(file) = file_of_prev(self.dir, path) else { return Ok(()) };
        let read = |p: &Path| hash_file(p).map(|h| h.map(|(d, _)| d)).map_err(|why| os_err(CODE_IO, &why));
        let copy = read(path)?;
        if copy.is_none() || read(&file)? == copy {
            Ok(())
        } else {
            let what = format!("update journal: {} holds the only copy of the file its run replaced", name_of(path));
            Err(os_err(CODE_ACCESS_DENIED, &what))
        }
    }
}

fn os_err(code: u32, what: &str) -> OsErr {
    OsErr { code, what: what.to_string() }
}

fn files_only() -> OsErr {
    os_err(CODE_ACCESS_DENIED, "update journal: file changes only")
}

impl JournalOs for FolderOs<'_> {
    fn reg_get(&self, _: Hive, _: &RegPath, _: Option<&str>) -> Result<Option<RegData>, OsErr> {
        Err(files_only())
    }
    fn reg_set(&mut self, _: Hive, _: &RegPath, _: Option<&str>, _: Option<&RegData>) -> Result<(), OsErr> {
        Err(files_only())
    }
    fn reg_key_exists(&self, _: Hive, _: &RegPath) -> Result<bool, OsErr> {
        Err(files_only())
    }
    fn reg_key_create(&mut self, _: Hive, _: &RegPath) -> Result<(), OsErr> {
        Err(files_only())
    }
    fn reg_key_delete_if_empty(&mut self, _: Hive, _: &RegPath) -> Result<bool, OsErr> {
        Err(files_only())
    }
    fn reg_tree_get(&self, _: Hive, _: &RegPath) -> Result<Option<RegSnapshot>, OsErr> {
        Err(files_only())
    }
    fn reg_tree_put(&mut self, _: Hive, _: &RegPath, _: Option<&RegSnapshot>) -> Result<(), OsErr> {
        Err(files_only())
    }
    fn task_xml(&self, _: &TaskName) -> Result<Option<String>, OsErr> {
        Err(files_only())
    }
    fn task_put(&mut self, _: &TaskName, _: Option<&str>) -> Result<(), OsErr> {
        Err(files_only())
    }
    fn file_digest(&self, path: &str) -> Result<Option<Digest>, OsErr> {
        let p = self.ours(path)?;
        hash_file(&p).map(|h| h.map(|(d, _)| d)).map_err(|why| os_err(CODE_IO, &why))
    }
    /// A file in use (a running image) is renamed to `.old-<n>` and deleted at the next restart: the name is free.
    fn file_delete(&mut self, path: &str) -> Result<(), OsErr> {
        let p = self.ours(path)?;
        self.not_the_only_copy(&p)?;
        match std::fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                let old = old_name(&p).ok_or_else(|| os_err(CODE_IO, "update journal: no free .old name"))?;
                rename(&p, &old).map_err(|why| os_err(CODE_IO, &format!("{}: {e}; {why}", name_of(&p))))?;
                discard(&old, &mut *self.env);
                Ok(())
            }
        }
    }
    /// A copy of `from` (which stays, so a second undo finds dest equal to it) through `<dest>.new`, flushed, then
    /// put in place as a swap is: the name always resolves, even over a running image.
    fn file_restore(&mut self, dest: &str, from: &str) -> Result<(), OsErr> {
        let (d, f) = (self.ours(dest)?, self.ours(from)?);
        self.not_the_only_copy(&d)?;
        let new = with_suffix(&d, ".new");
        copy_flushed(&f, &new).map_err(|why| os_err(CODE_IO, &why))?;
        put_in_place(&d, &new, &mut *self.env).map_err(|why| os_err(CODE_IO, &why))
    }
    fn digest(&self, bytes: &[u8]) -> Result<Digest, OsErr> {
        oled_sys::sha256::sha256(bytes).map_err(|why| os_err(CODE_IO, &why))
    }
}

/// Deletes what swaps leave once their runs are closed: the `.new` staging names and the `.old-<n>` copies of these
/// components (a copy still in use stays: its delete at restart was asked for when it was made), then `lang\\` when
/// it is a real, empty folder.
fn sweep(dir: &Path, ids: &[ComponentId]) {
    for id in ids.iter().filter(|c| **c != ComponentId::Dock) {
        for name in [id.file(), id.prev()] {
            let base = dir.join(name);
            let _ = remove_stale(&with_suffix(&base, ".new"));
            let (Some(folder), Some(file)) = (base.parent(), base.file_name().and_then(|f| f.to_str())) else {
                continue;
            };
            let Ok(entries) = std::fs::read_dir(folder) else { continue };
            for e in entries.flatten() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                let old = n
                    .strip_prefix(file.to_lowercase().as_str())
                    .and_then(|r| r.strip_prefix(".old-"))
                    .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()));
                if old && e.file_type().is_ok_and(|t| t.is_file()) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let lang = dir.join(oled_trust::manifest::suite::LANG_DIR);
    if std::fs::symlink_metadata(&lang).is_ok_and(|m| m.is_dir() && !is_reparse(&m)) {
        let _ = std::fs::remove_dir(&lang); // fails while it holds anything
    }
}

fn is_reparse(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

/// `dir` as a real folder (not a link), created when missing (it then inherits its parent's ACL).
fn real_dir(dir: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() && !is_reparse(&m) => Ok(()),
        Ok(_) => Err(format!("{} is not a real folder", name_of(dir))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(dir).map_err(|e| format!("creating {}: {e}", name_of(dir)))
        }
        Err(e) => Err(format!("{}: {e}", name_of(dir))),
    }
}

/// The file's SHA-256 and size (at most COMPONENT_MAX_BYTES, read without following a link); None when it is missing.
fn hash_file(path: &Path) -> Result<Option<(Digest, u64)>, String> {
    match oled_sys::datadir::read_file(path, COMPONENT_MAX_BYTES, false) {
        Ok(bytes) => {
            #[cfg(test)]
            HASHED.with(|n| n.set(n.get() + 1));
            oled_sys::sha256::sha256(&bytes).map(|h| Some((h, bytes.len() as u64)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", name_of(path))),
    }
}

#[cfg(test)]
thread_local! {
    /// Files `hash_file` read and hashed on this thread (converged_run_reads_no_network counts them).
    static HASHED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn check_hash(path: &Path, want: Digest) -> Result<(), String> {
    match hash_file(path)? {
        Some((h, _)) if h == want => Ok(()),
        _ => Err(format!("{} is not the release's file", name_of(path))),
    }
}

/// The file name alone, for messages (never a full path: house rule 6).
fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn path_text(p: &Path) -> Result<String, String> {
    p.to_str().map(str::to_string).ok_or_else(|| format!("{}: not a Unicode path", name_of(p)))
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// The first `<path>.old-<n>` that does not exist (n from 1).
fn old_name(path: &Path) -> Option<PathBuf> {
    (1..=OLD_MAX).map(|n| with_suffix(path, &format!(".old-{n}"))).find(|p| std::fs::symlink_metadata(p).is_err())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Deletes a file a crash may have left at `p` (a missing one is fine).
fn remove_stale(p: &Path) -> Result<(), String> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", name_of(p))),
    }
}

/// `from` copied to `to` (a stale `to` deleted first) and flushed to the disk (FlushFileBuffers).
fn copy_flushed(from: &Path, to: &Path) -> Result<(), String> {
    remove_stale(to)?;
    std::fs::copy(from, to).map_err(|e| format!("copying {} to {}: {e}", name_of(from), name_of(to)))?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(to)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("flushing {}: {e}", name_of(to)))
}

/// `from` renamed to the free name `to`, written through.
fn rename(from: &Path, to: &Path) -> Result<(), String> {
    let (f, t) = (HSTRING::from(from.as_os_str()), HSTRING::from(to.as_os_str()));
    // SAFETY: two NUL-terminated paths that outlive the call.
    unsafe { MoveFileExW(&f, &t, MOVEFILE_WRITE_THROUGH) }
        .map_err(|e| format!("renaming {} to {}: {e}", name_of(from), name_of(to)))
}

/// `new` takes the name `dest`, whose file moves to the free name `backup`, by the rule U1s measured: one
/// ReplaceFileW (the name always resolves) or the rename pair.
fn replace_keeping(dest: &Path, new: &Path, backup: &Path) -> Result<(), String> {
    match SWAP {
        Swap::Replace => {
            let (d, n, b) =
                (HSTRING::from(dest.as_os_str()), HSTRING::from(new.as_os_str()), HSTRING::from(backup.as_os_str()));
            // SAFETY: three NUL-terminated paths that outlive the call; no exclude or reserved data.
            let r = unsafe { ReplaceFileW(&d, &n, &b, REPLACEFILE_IGNORE_MERGE_ERRORS, None, None) };
            r.map_err(|e| {
                // The replaced file is at the backup name already and the replacement could not take its place: the
                // replaced file goes back, so the name resolves again.
                if e.code() == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2.to_hresult()
                    && std::fs::symlink_metadata(dest).is_err()
                {
                    let _ = rename(backup, dest);
                }
                format!("replacing {}: {e}", name_of(dest))
            })
        }
        Swap::RenamePair => {
            rename(dest, backup)?;
            rename(new, dest).inspect_err(|_| {
                let _ = rename(backup, dest);
            })
        }
    }
}

/// `new` put at `dest`: over an existing file by `replace_keeping` (the replaced file then deleted, or deleted at
/// the next restart when it is a running image), to a free name by a rename.
fn put_in_place(dest: &Path, new: &Path, env: &mut dyn SwapEnv) -> Result<(), String> {
    if std::fs::symlink_metadata(dest).is_err() {
        return rename(new, dest);
    }
    let old = old_name(dest).ok_or_else(|| format!("{}: no free .old name", name_of(dest)))?;
    replace_keeping(dest, new, &old)?;
    discard(&old, env);
    Ok(())
}

/// The bytes of `from`, which must hash to `want`, put back at `dest` through `<dest>.new`, flushed, as a swap puts a
/// file in place (the name always resolves).
fn put_back(dest: &Path, from: &Path, want: Digest, env: &mut dyn SwapEnv) -> Result<(), String> {
    match hash_file(from)? {
        Some((h, _)) if h == want => {}
        _ => return Err(format!("{} does not hold the file to put back", name_of(from))),
    }
    let new = with_suffix(dest, ".new");
    copy_flushed(from, &new)?;
    put_in_place(dest, &new, env)
}

/// A replaced file deleted now, or at the next restart when it is in use (best effort: a sweep tries again later).
fn discard(old: &Path, env: &mut dyn SwapEnv) {
    if std::fs::remove_file(old).is_err() && std::fs::symlink_metadata(old).is_ok() {
        let _ = env.delete_at_restart(old);
    }
}

// ---------------------------------------------------------------------------------------------- U1d: selftests

/// What a component's proof said (U1d): its selftest at medium (F-P7) or, for the language file, its parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Selftest {
    /// It proved itself: the caller commits its run.
    Passed,
    /// It did not: the caller reverts its run and gives the hash a strike.
    Failed,
    /// No proof could be asked for (the shell or the event were not available, or the event name was taken, ROADMAP
    /// R6): the caller reverts this run's components with no strike, and the next start tries again.
    Unavailable(String),
}

/// How long a component gets to signal (F-P7 gives the app 10 s; the start through the shell comes on top).
pub(crate) const SELFTEST_WAIT: Duration = Duration::from_secs(15);

/// The event a selftest signals, as the dock waits on it.
pub(crate) trait SelftestWait {
    fn wait(&self, timeout: Duration) -> bool;
}

impl SelftestWait for oled_sys::selftest::Waiter {
    fn wait(&self, timeout: Duration) -> bool {
        oled_sys::selftest::Waiter::wait(self, timeout)
    }
}

/// Why no selftest event could be made.
#[derive(Debug)]
pub(crate) enum WaiterError {
    /// The name exists already: another process made it first (never trusted, never a pass).
    Squatted,
    Other(String),
}

/// The two OS parts of a selftest, so tests stand in for them.
pub(crate) trait SelftestHost {
    type Waiter: SelftestWait;
    /// A fresh event for `app`'s selftest that the user the app runs as may signal, and its nonce.
    fn waiter(&mut self, app: &str) -> Result<(Self::Waiter, String), WaiterError>;
    /// Starts `exe` with `params` at the user's medium integrity, hidden.
    fn start(&mut self, exe: &Path, params: &str) -> Result<(), String>;
}

/// The real host: Explorer's user may signal the event (under Administrator protection it is not the dock's
/// account), and the exe starts through the desktop shell (`desktop_shell_execute`), so it runs at medium even
/// from the elevated sync. Both need a COM STA thread: the sync's own (U1e).
pub(crate) struct DesktopHost;

impl SelftestHost for DesktopHost {
    type Waiter = oled_sys::selftest::Waiter;
    fn waiter(&mut self, app: &str) -> Result<(Self::Waiter, String), WaiterError> {
        let pid = crate::shell_thread::explorer_pid().map_err(|e| WaiterError::Other(format!("no Explorer: {e}")))?;
        let sid =
            oled_sys::pipe::user_sid_of_pid(pid).map_err(|e| WaiterError::Other(format!("Explorer's user: {e}")))?;
        oled_sys::selftest::Waiter::create(app, "selftest", &sid).map_err(|e| match e {
            oled_sys::pipe::PipeError::Squatted => WaiterError::Squatted,
            other => WaiterError::Other(format!("the selftest event: {other}")),
        })
    }
    fn start(&mut self, exe: &Path, params: &str) -> Result<(), String> {
        let file = exe.to_str().ok_or_else(|| format!("{}: not a Unicode path", name_of(exe)))?;
        let hide = windows::Win32::UI::WindowsAndMessaging::SW_HIDE.0;
        crate::shell_thread::desktop_shell_execute(file, Some(params), None, Verb::Open, hide)
            .map_err(|e| format!("starting {} through the shell: {e}", name_of(exe)))
    }
}

/// F-P7 from the dock's side: `<dir>\<app>.exe --selftest --signal <nonce>` started at medium, then its event waited
/// for, at most SELFTEST_WAIT. The app answers beside a running instance of itself (before any mutex, never
/// forwarding, no window). The dock and the language file have no selftest.
pub(crate) fn run_selftest<H: SelftestHost>(dir: &Path, id: ComponentId, host: &mut H) -> Selftest {
    if matches!(id, ComponentId::Dock | ComponentId::LangIt) {
        return Selftest::Unavailable(format!("{} has no selftest", id.id()));
    }
    let (waiter, nonce) = match host.waiter(id.id()) {
        Ok(w) => w,
        Err(WaiterError::Squatted) => return Selftest::Unavailable("the selftest event's name is taken".into()),
        Err(WaiterError::Other(why)) => return Selftest::Unavailable(why),
    };
    if let Err(why) = host.start(&dir.join(id.file()), &format!("--selftest --signal {nonce}")) {
        return Selftest::Unavailable(why);
    }
    if waiter.wait(SELFTEST_WAIT) {
        Selftest::Passed
    } else {
        Selftest::Failed
    }
}

/// The language file's proof: the file in place, read through the checked reader (at most LANG_MAX_BYTES), passes
/// `checker` (U1e: oled_sys::lang::LangFile::parse, the tag "it" and the manifest's version).
pub(crate) fn check_lang(dir: &Path, id: ComponentId, checker: &dyn Fn(&[u8]) -> Result<(), String>) -> Selftest {
    let cap = oled_sys::lang::LANG_MAX_BYTES as u64;
    match oled_sys::datadir::read_file(&dir.join(id.file()), cap, false) {
        Ok(bytes) if checker(&bytes).is_ok() => Selftest::Passed,
        _ => Selftest::Failed,
    }
}

// ---------------------------------------------------------------------------------------------- U1e: --suite-sync

/// `--suite-sync`'s exit codes (2, bad arguments, is main's): ok or pending; a component failed its proof or a step
/// failed; try again later (another sync runs, or no selftest could be asked for); not this release's dock, or a
/// release without a manifest.
pub(crate) const EXIT_OK: i32 = 0;
pub(crate) const EXIT_FAILED: i32 = 1;
pub(crate) const EXIT_RETRY: i32 = 3;
pub(crate) const EXIT_REFUSED: i32 = 4;
/// The one line the sync prints and the dock logs: at most RESULT_MAX bytes, starting with RESULT_PREFIX.
pub(crate) const RESULT_PREFIX: &str = "suite-sync: ";
pub(crate) const RESULT_MAX: usize = 512;
/// Failed proofs of one build of a component after which the sync skips that build until the release has another.
const STRIKES: u32 = 3;
/// The most a strike file (`bad-<id>` in the stage: "<sha256 hex> <count>\n") is read.
const STRIKE_MAX_BYTES: u64 = 128;
/// Win32 ERROR_SHARING_VIOLATION: update.jnl or the sync's lock is held by another process.
const CODE_SHARING: u32 = 32;

/// What a sync ended with: its exit code and its result line (without the time, which `run_sync` adds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncResult {
    pub code: i32,
    pub line: String,
}

impl SyncResult {
    fn new(code: i32, state: &str, why: &str) -> SyncResult {
        SyncResult { code, line: format!("{RESULT_PREFIX}{state} ({why})") }
    }
}

fn refused(why: &str) -> SyncResult {
    SyncResult::new(EXIT_REFUSED, "refused", why)
}

/// The words of the "not this release's dock" refusal.
const NOT_PUBLISHED: &str = "this dock.exe is not the published build";

/// The release's side of a sync (tests stand in for it).
pub(crate) trait SuiteNet {
    /// The release of a canonical tag (U1b: through the `--update-fetch <tag>` child).
    fn release(&mut self, tag: &str) -> Result<Release, String>;
    /// One asset: status 200, from an allowed host, exactly `asset.size` bytes (at most `cap`).
    fn get(&mut self, asset: &Asset, cap: u64) -> Result<Vec<u8>, String>;
}

/// The stage of one release, `<root>\update\<tag>\` (tests: a temp folder).
pub(crate) trait SuiteStage {
    /// A staged file (at most `cap` bytes); NotFound when it is not staged.
    fn read(&mut self, name: &str, cap: u64) -> std::io::Result<Vec<u8>>;
    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// The checked path of a staged file, which a swap copies from.
    fn path(&mut self, name: &str) -> std::io::Result<PathBuf>;
    /// The whole stage, once its job is done.
    fn remove(&mut self) -> Result<(), String>;
}

/// What a sync does outside the install folder and the stage (tests stand in for it).
pub(crate) trait SyncOs {
    /// The component's selftest at medium (U1d).
    fn selftest(&mut self, dir: &Path, id: ComponentId) -> Selftest;
    /// The Add/Remove Programs entry, its EstimatedSize summed over the installed components (`arp::write`).
    fn write_arp(&mut self, dir: &Path) -> Result<(), String>;
    /// Tells a running copy of the app that its exe was swapped (F-P7's "restart to apply").
    fn notify_updated(&mut self, dir: &Path, id: ComponentId, v: Version);
}

/// What one sync works on.
pub(crate) struct SyncCtx<'a> {
    /// The install folder.
    pub dir: &'a Path,
    /// update.jnl.
    pub journal: &'a Path,
    /// The dock's own version: the release whose components the install follows.
    pub version: Version,
    /// The sync may fetch the release and download what the install folder and the stage do not hold.
    pub network: bool,
}

/// Where the manifest of this run came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManifestFrom {
    Installed,
    Staged,
    Fetched,
}

/// What the steps of one sync did, for its result line.
#[derive(Default)]
struct Tally {
    added: Vec<ComponentId>,
    replaced: Vec<ComponentId>,
    reverted: Vec<ComponentId>,
    pending: Vec<ComponentId>,
    /// Components taken from their previous copy, and downloaded; the release was asked for.
    from_prev: usize,
    downloaded: Vec<ComponentId>,
    fetched: bool,
    /// A component failed its proof, or a step failed.
    failed: bool,
    /// No proof could be asked for: the run stopped.
    unavailable: bool,
    notes: Vec<String>,
}

impl Tally {
    fn changed(&self) -> bool {
        !self.added.is_empty() || !self.replaced.is_empty()
    }

    fn postpone(&mut self, id: ComponentId, why: &str) {
        self.pending.push(id);
        self.notes.push(format!("{}: {why}", id.id()));
    }

    fn fail(&mut self, id: ComponentId, why: &str) {
        self.reverted.push(id);
        self.failed = true;
        self.notes.push(format!("{}: {why}", id.id()));
    }

    fn state(&self) -> (i32, &'static str) {
        if self.unavailable {
            (EXIT_RETRY, "unavailable")
        } else if self.failed {
            (EXIT_FAILED, "failed")
        } else if !self.pending.is_empty() {
            (EXIT_OK, "pending")
        } else {
            (EXIT_OK, "ok")
        }
    }

    /// "suite-sync: <state>[ added=..][ replaced=..][ reverted=..][ pending=..][ (<notes>)]".
    fn result(&self) -> SyncResult {
        let (code, state) = self.state();
        let mut line = format!("{RESULT_PREFIX}{state}");
        for (key, ids) in [
            ("added", &self.added),
            ("replaced", &self.replaced),
            ("reverted", &self.reverted),
            ("pending", &self.pending),
        ] {
            if !ids.is_empty() {
                line.push_str(&format!(" {key}={}", ids_text(ids)));
            }
        }
        let mut notes: Vec<String> = Vec::new();
        let changed = self.added.len() + self.replaced.len();
        if changed > 0 && self.from_prev == changed {
            notes.push("from prev".into());
        }
        if !self.downloaded.is_empty() {
            notes.push(format!("downloaded {}", ids_text(&self.downloaded)));
        }
        if self.fetched {
            notes.push("release fetched".into());
        }
        notes.extend(self.notes.iter().cloned());
        if !notes.is_empty() {
            line.push_str(&format!(" ({})", notes.join("; ")));
        }
        SyncResult { code, line }
    }
}

fn ids_text(ids: &[ComponentId]) -> String {
    ids.iter().map(|c| c.id()).collect::<Vec<_>>().join(",")
}

/// update.jnl under its lock, opened when a replay or a swap needs it (a converged run opens nothing).
type OpenJournal = Option<(Journal, FileSink)>;

/// One sync (U1e), after the caller took the sync's lock: update.jnl's uncommitted runs put back; the manifest of this
/// release for this dock.exe (the install folder's suite.json, else the stage's, else the release's when the network
/// may be used); each component other than the dock brought to it (from its previous copy, the stage or a download),
/// proved (a selftest at medium, or the language file's parse) and committed, or put back; then, when a file changed,
/// suite.json, the ARP entry and the running apps told. A converged install reads no network and writes nothing.
pub(crate) fn sync_in(
    cx: &SyncCtx<'_>,
    net: &mut dyn SuiteNet,
    stage: &mut dyn SuiteStage,
    os: &mut dyn SyncOs,
    env: &mut dyn SwapEnv,
) -> SyncResult {
    let mut open: OpenJournal = None;
    let r = sync_steps(cx, net, stage, os, env, &mut open);
    // update.jnl goes once nothing in it is open; a revert that failed keeps it for the next sync.
    if let Some((j, sink)) = open {
        let _ = sink.delete_if_settled(&j);
    }
    r
}

fn sync_steps(
    cx: &SyncCtx<'_>,
    net: &mut dyn SuiteNet,
    stage: &mut dyn SuiteStage,
    os: &mut dyn SyncOs,
    env: &mut dyn SwapEnv,
    open: &mut OpenJournal,
) -> SyncResult {
    let dir = cx.dir;
    // What an earlier sync left uncommitted (a crash, a kill, a power loss) is put back first.
    if std::fs::symlink_metadata(cx.journal).is_ok() {
        let (mut j, mut sink) = match open_journal(cx.journal) {
            Ok(o) => o,
            Err(r) => return r,
        };
        replay_open_in(dir, &mut j, &mut sink, env);
        *open = Some((j, sink));
    }
    let dock = match hash_file(&dir.join(ComponentId::Dock.file())) {
        Ok(Some((h, _))) => h,
        Ok(None) => return refused("no dock.exe in the install folder"),
        Err(why) => return SyncResult::new(EXIT_FAILED, "error", &why),
    };
    // The release, asked for at most once a run (a failed fetch is not tried again for the next component).
    let mut release: Option<Result<Release, String>> = None;
    let (own, from) = match own_manifest(cx, dock, net, stage, &mut release) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let mut tally = Tally { fetched: from == ManifestFrom::Fetched, ..Tally::default() };
    // The disk: one hash per installed component; the previous copy, the stage and the strikes only for a component
    // that is not in place.
    let (mut disk, mut bad, mut unreadable) = (Vec::new(), Vec::new(), Vec::new());
    for id in ComponentId::ALL.into_iter().filter(|c| *c != ComponentId::Dock) {
        let current = match hash_file(&dir.join(id.file())) {
            Ok(h) => h.map(|(h, _)| h),
            Err(why) => {
                unreadable.push((id, why));
                continue;
            }
        };
        let (mut prev, mut staged) = (None, None);
        if let Some(want) = own.components.iter().find(|c| c.id == id).map(|c| c.sha256).filter(|w| current != Some(*w))
        {
            prev = hash_file(&dir.join(id.prev())).ok().flatten().map(|(h, _)| h);
            staged = stage.read(id.asset(), COMPONENT_MAX_BYTES).ok().and_then(|b| oled_sys::sha256::sha256(&b).ok());
            if strikes(stage, id, want) >= STRIKES {
                bad.push((id, want));
            }
        }
        disk.push(OnDisk { id, current, prev, staged });
    }
    let steps = plan_convergence(&own, &disk, &bad, cx.network);
    for step in steps.into_iter().filter(|s| !unreadable.iter().any(|(u, _)| *u == step_id(*s))) {
        let (id, src, replace) = match step {
            Step::Keep(_) => continue,
            Step::Skip(id) => {
                let skipped = bad.iter().any(|(b, _)| *b == id);
                tally
                    .postpone(id, if skipped { "skipped after three failed proofs" } else { "not staged, no network" });
                continue;
            }
            Step::Add(id, s) => (id, s, false),
            Step::Replace(id, s) => (id, s, true),
        };
        let Some(comp) = own.components.iter().find(|c| c.id == id) else { continue };
        let staged = match source_path(cx, comp, src, net, stage, &mut release, &mut tally) {
            Ok(p) => p,
            Err(why) => {
                tally.postpone(id, &why);
                continue;
            }
        };
        if open.is_none() {
            match open_journal(cx.journal) {
                Ok(o) => *open = Some(o),
                Err(r) => return r, // before the first swap: nothing was touched
            }
        }
        let Some((j, sink)) = open.as_mut() else { continue };
        let swapped = match swap_in(dir, comp, &staged, j, sink, env) {
            Ok(s) => s,
            Err(SwapError::Staged(why)) => {
                tally.postpone(id, &why);
                continue;
            }
            Err(e) => {
                let (run, why) = match e {
                    SwapError::Failed { run, why } => (run, why),
                    SwapError::Stopped { run, step } => (run, format!("stopped after step {step}")),
                    SwapError::Dock | SwapError::Staged(_) => (None, "the dock is never swapped".to_string()),
                };
                if let Some(run) = run {
                    note_revert(&mut tally, id, revert(dir, id, run, j, sink, env));
                }
                tally.fail(id, &why);
                continue;
            }
        };
        let run = swapped.run;
        let proof = match id {
            ComponentId::LangIt => check_lang(dir, id, &|b: &[u8]| lang_matches(b, own.version)),
            _ => os.selftest(dir, id),
        };
        match proof {
            Selftest::Passed => match commit_swap(j, sink, swapped, env) {
                Ok(()) => {
                    if replace {
                        tally.replaced.push(id)
                    } else {
                        tally.added.push(id)
                    }
                    tally.from_prev += usize::from(src == Source::Prev);
                    if src == Source::Download {
                        tally.downloaded.push(id);
                    }
                }
                Err(e) => {
                    note_revert(&mut tally, id, revert(dir, id, run, j, sink, env));
                    tally.fail(id, &format!("update.jnl: {}", e.what));
                }
            },
            Selftest::Failed => {
                note_revert(&mut tally, id, revert(dir, id, run, j, sink, env));
                let n = strike(stage, id, comp.sha256);
                let what = if id == ComponentId::LangIt { "the language file's check" } else { "its selftest" };
                tally.fail(id, &format!("{what} failed, strike {n}"));
            }
            Selftest::Unavailable(why) => {
                // No proof now (Explorer down, the event's name taken): put back, no strike, the next start tries again.
                note_revert(&mut tally, id, revert(dir, id, run, j, sink, env));
                tally.reverted.push(id);
                tally.unavailable = true;
                tally.notes.push(format!("no selftest: {why}"));
                break;
            }
        }
    }
    for (id, why) in &unreadable {
        tally.postpone(*id, why);
    }
    // The manifest this install follows, so the next start needs neither the stage nor the network.
    if from != ManifestFrom::Installed {
        if let Err(why) = write_installed_manifest(dir, &own) {
            tally.notes.push(why);
        }
    }
    if tally.changed() {
        if let Err(why) = os.write_arp(dir) {
            tally.notes.push(format!("ARP entry: {why}"));
        }
        for id in tally.added.iter().chain(&tally.replaced) {
            os.notify_updated(dir, *id, own.version);
        }
    }
    let r = tally.result();
    if tally.changed() && tally.state().1 == "ok" {
        let _ = stage.remove(); // every component in place: the stage's job is done
    }
    r
}

fn step_id(s: Step) -> ComponentId {
    match s {
        Step::Keep(id) | Step::Add(id, _) | Step::Replace(id, _) | Step::Skip(id) => id,
    }
}

/// update.jnl under its lock (a writer that holds it longer than LOCK_WAIT: try again later).
fn open_journal(path: &Path) -> Result<(Journal, FileSink), SyncResult> {
    oled_sys::journal_os::open_locked_at(path, journal::Scope::Machine, LOCK_WAIT).map_err(|e| {
        if e.code == CODE_SHARING {
            SyncResult::new(EXIT_RETRY, "busy", "update.jnl is held by another process")
        } else {
            SyncResult::new(EXIT_FAILED, "error", &format!("update.jnl: {}", e.what))
        }
    })
}

/// A revert that left something open (another program's file, an error) is said in the line; update.jnl keeps the
/// entry and the next sync tries again.
fn note_revert(tally: &mut Tally, id: ComponentId, r: UndoReport) {
    if !r.failed.is_empty() || !r.changed_by_other.is_empty() {
        tally.notes.push(format!("{}: not all put back ({} left)", id.id(), r.failed.len() + r.changed_by_other.len()));
    }
}

/// The manifest of the dock's own release for this dock.exe, first match: the install folder's suite.json; the
/// stage's; with the network, the release's (fetched by tag, its suite.json downloaded, checked and staged). Each must
/// be of this version and list the installed dock.exe's SHA-256 as its dock: a dock.exe that is not the published
/// build (a dev build) touches nothing.
fn own_manifest(
    cx: &SyncCtx<'_>,
    dock: Digest,
    net: &mut dyn SuiteNet,
    stage: &mut dyn SuiteStage,
    release: &mut Option<Result<Release, String>>,
) -> Result<(SuiteManifest, ManifestFrom), SyncResult> {
    let for_this_dock = |m: &SuiteManifest| {
        m.version == cx.version && m.components.iter().any(|c| c.id == ComponentId::Dock && c.sha256 == dock)
    };
    let installed = oled_sys::datadir::read_file(&cx.dir.join(SUITE_ASSET), SUITE_MAX_BYTES, false);
    if let Some(m) = installed.ok().and_then(|b| parse_manifest(&b).ok()).filter(|m| for_this_dock(m)) {
        return Ok((m, ManifestFrom::Installed));
    }
    let mut other_dock = false;
    if let Some(m) = stage.read(SUITE_ASSET, SUITE_MAX_BYTES).ok().and_then(|b| parse_manifest(&b).ok()) {
        if for_this_dock(&m) {
            return Ok((m, ManifestFrom::Staged));
        }
        other_dock = m.version == cx.version;
    }
    if !cx.network {
        return Err(if other_dock {
            refused(NOT_PUBLISHED)
        } else {
            SyncResult::new(EXIT_OK, "pending", "no manifest of this release: not staged, no network")
        });
    }
    let rel = net
        .release(&cx.version.tag())
        .map_err(|why| SyncResult::new(EXIT_OK, "pending", &format!("the release: {why}")))?;
    let Some(asset) = rel.suite.clone() else { return Err(refused("the release has no suite.json")) };
    let bytes = fetch_checked(net, &asset, SUITE_MAX_BYTES, None)
        .map_err(|why| SyncResult::new(EXIT_OK, "pending", &format!("suite.json: {why}")))?;
    let m = parse_manifest(&bytes).map_err(|why| refused(&format!("the release's {why}")))?;
    if m.version != cx.version {
        return Err(refused("the release's suite.json is of another version"));
    }
    let _ = stage.write(SUITE_ASSET, &bytes); // the next sync reads the stage; this one has the bytes
    *release = Some(Ok(rel));
    if !for_this_dock(&m) {
        return Err(refused(NOT_PUBLISHED));
    }
    Ok((m, ManifestFrom::Fetched))
}

/// The file a swap copies from: the component's previous copy, its staged copy, or a download (checked against the
/// manifest and GitHub's digest, then staged).
fn source_path(
    cx: &SyncCtx<'_>,
    comp: &Component,
    src: Source,
    net: &mut dyn SuiteNet,
    stage: &mut dyn SuiteStage,
    release: &mut Option<Result<Release, String>>,
    tally: &mut Tally,
) -> Result<PathBuf, String> {
    let name = comp.id.asset();
    match src {
        Source::Prev => return Ok(cx.dir.join(comp.id.prev())),
        Source::Stage => {}
        Source::Download => {
            if release.is_none() {
                *release = Some(net.release(&cx.version.tag()));
                tally.fetched = true;
            }
            let rel = match release {
                Some(Ok(r)) => r,
                Some(Err(why)) => return Err(format!("the release: {why}")),
                None => return Err("the release: not fetched".into()),
            };
            let asset = rel
                .component_assets
                .iter()
                .find(|(c, _)| *c == comp.id)
                .map(|(_, a)| a.clone())
                .ok_or("not among the release's assets")?;
            if asset.size != comp.size {
                return Err("the release's asset is not the size suite.json gives".into());
            }
            let bytes = fetch_checked(net, &asset, COMPONENT_MAX_BYTES, Some(comp.sha256))?;
            stage.write(name, &bytes).map_err(|why| format!("staging it: {why}"))?;
        }
    }
    stage.path(name).map_err(|e| format!("the stage: {e}"))
}

/// An asset downloaded and checked: exactly its announced size, the manifest's SHA-256 when `want` has one, and
/// GitHub's digest when it sent one.
fn fetch_checked(net: &mut dyn SuiteNet, asset: &Asset, cap: u64, want: Option<Digest>) -> Result<Vec<u8>, String> {
    let bytes = net.get(asset, cap)?;
    if bytes.len() as u64 != asset.size {
        return Err(format!("{} bytes, expected {}", bytes.len(), asset.size));
    }
    let got = oled_sys::sha256::sha256(&bytes)?;
    if want.is_some_and(|w| w != got) {
        return Err("not the file suite.json names (SHA-256)".into());
    }
    if asset.digest.is_some_and(|d| d != got) {
        return Err("SHA-256 differs from GitHub's digest of the asset".into());
    }
    Ok(bytes)
}

/// The language file's proof: it parses (oled_sys::lang, total and capped), its tag is "it" and it is of this release.
fn lang_matches(bytes: &[u8], v: Version) -> Result<(), String> {
    let f = oled_sys::lang::LangFile::parse(bytes.to_vec()).map_err(|e| format!("{e:?}"))?;
    let want = (u16::try_from(v.major), u16::try_from(v.minor), u16::try_from(v.patch));
    let ok = match want {
        (Ok(a), Ok(b), Ok(c)) => f.tag() == "it" && f.suite_version() == (a, b, c),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err("another language or release".into())
    }
}

fn strike_name(id: ComponentId) -> String {
    format!("bad-{}", id.id())
}

/// The failed proofs recorded for this build (`hash`) of `id` in the stage; 0 for another build or none.
fn strikes(stage: &mut dyn SuiteStage, id: ComponentId, hash: Digest) -> u32 {
    let Ok(bytes) = stage.read(&strike_name(id), STRIKE_MAX_BYTES) else { return 0 };
    let text = String::from_utf8_lossy(&bytes);
    let mut words = text.split_whitespace();
    match (words.next(), words.next().and_then(|n| n.parse::<u32>().ok())) {
        (Some(h), Some(n)) if h.eq_ignore_ascii_case(&hex(&hash)) => n,
        _ => 0,
    }
}

/// One more failed proof for this build of `id`; returns the new count.
fn strike(stage: &mut dyn SuiteStage, id: ComponentId, hash: Digest) -> u32 {
    let n = strikes(stage, id, hash).saturating_add(1).min(999);
    let _ = stage.write(&strike_name(id), format!("{} {n}\n", hex(&hash)).as_bytes());
    n
}

fn hex(h: &Digest) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// `<install dir>\suite.json` = encode_manifest(own), written (temp file, flushed, renamed over) when missing or
/// different.
fn write_installed_manifest(dir: &Path, own: &SuiteManifest) -> Result<(), String> {
    let text = encode_manifest(own);
    let path = dir.join(SUITE_ASSET);
    if oled_sys::datadir::read_file(&path, SUITE_MAX_BYTES, false).is_ok_and(|b| b == text.as_bytes()) {
        return Ok(());
    }
    oled_sys::datadir::write_atomic(&path, text.as_bytes(), oled_sys::datadir::FileAcl::Inherit)
        .map_err(|_| format!("{SUITE_ASSET} not written"))
}

// ---------------------------------------------------------------------------------------------- U1e: the real run

/// The network of the sync child: the release through the `--update-fetch <tag>` child, assets by WinHTTP.
pub(crate) struct RealNet;

impl SuiteNet for RealNet {
    fn release(&mut self, tag: &str) -> Result<Release, String> {
        super::fetch_release(Some(tag), true).map_err(|e| e.to_string())
    }
    fn get(&mut self, asset: &Asset, cap: u64) -> Result<Vec<u8>, String> {
        super::get_asset(asset, cap)
    }
}

/// `<root>\update\<tag>\` through datadir's checked stage functions (admin-only, no link followed).
pub(crate) struct DataStage {
    pub tag: String,
}

impl SuiteStage for DataStage {
    fn read(&mut self, name: &str, cap: u64) -> std::io::Result<Vec<u8>> {
        crate::datadir::read_stage_file(&self.tag, name, cap)
    }
    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        crate::datadir::write_stage_file(&self.tag, name, bytes)
    }
    fn path(&mut self, name: &str) -> std::io::Result<PathBuf> {
        crate::datadir::checked_stage_path(&self.tag, name)
    }
    fn remove(&mut self) -> Result<(), String> {
        crate::datadir::remove_update_stage(&self.tag)
    }
}

/// The machine: selftests through the desktop shell, the ARP entry, the apps' windows.
struct MachineOs;

impl SyncOs for MachineOs {
    fn selftest(&mut self, dir: &Path, id: ComponentId) -> Selftest {
        run_selftest(dir, id, &mut DesktopHost)
    }
    fn write_arp(&mut self, dir: &Path) -> Result<(), String> {
        crate::arp::write(dir)
    }
    fn notify_updated(&mut self, dir: &Path, id: ComponentId, v: Version) {
        // F-P7: Tasks' top-level windows; Folio's message-only window, which top-level enumeration does not see.
        let (class, message_only) = match id {
            ComponentId::Tasks => ("MiniOledTasks", false),
            ComponentId::Files => ("MiniOledFilesIpc", true),
            _ => return,
        };
        oled_sys::selftest::notify_updated(class, message_only, &dir.join(id.file()), (v.major, v.minor, v.patch));
    }
}

/// `--suite-sync [--network]` (U1e): the elevated child the dock starts in its job once per start and after a
/// successful check. Prints one result line on stdout (the dock logs it) and exits 0 (ok or pending), 1 (a component
/// failed, or an error), 3 (try again later) or 4 (not this release's dock, or a release without a manifest).
pub fn run_sync(network: bool) -> i32 {
    let started = std::time::Instant::now();
    let r = sync_here(network);
    let line = finish_line(&r.line, started.elapsed());
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
    r.code
}

/// The line as printed: control characters as spaces, at most RESULT_MAX bytes with " in <n> ms" at its end.
pub(crate) fn finish_line(line: &str, took: Duration) -> String {
    let tail = format!(" in {} ms", took.as_millis());
    let mut s: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut room = RESULT_MAX.saturating_sub(tail.len()).min(s.len());
    while !s.is_char_boundary(room) {
        room -= 1;
    }
    s.truncate(room);
    s.push_str(&tail);
    s
}

fn sync_here(network: bool) -> SyncResult {
    let _ = crate::datadir::init();
    let (Some(lock), Some(jnl)) = (crate::datadir::update_lock_path(), crate::datadir::update_journal_path()) else {
        return SyncResult::new(EXIT_RETRY, "unavailable", "no data folder");
    };
    if !crate::datadir::writable() {
        return SyncResult::new(EXIT_RETRY, "unavailable", "the data folder is not writable");
    }
    let dir = crate::autostart::installed_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let Some(dir) = dir.filter(|d| std::fs::symlink_metadata(d).is_ok_and(|m| m.is_dir() && !is_reparse(&m))) else {
        return refused("no install folder");
    };
    let version = Version::current();
    let tag = version.tag();
    // The lock lives in <root>\update\, which the dock's first stage creates (admin-only); a first sync makes it.
    if lock.parent().is_some_and(|p| std::fs::symlink_metadata(p).is_err()) {
        if let Err(why) = crate::datadir::update_stage_dir(&tag) {
            return SyncResult::new(EXIT_RETRY, "unavailable", &format!("the update folder: {why}"));
        }
    }
    locked(&lock, || {
        // SAFETY: plain COM initialization of this thread (the selftests go through the shell, an STA's job); paired
        // with CoUninitialize below when it succeeded.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let cx = SyncCtx { dir: &dir, journal: &jnl, version, network };
        let r = sync_in(&cx, &mut RealNet, &mut DataStage { tag: tag.clone() }, &mut MachineOs, &mut RealEnv);
        if com.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above; every COM object of the run is released.
            unsafe { CoUninitialize() };
        }
        r
    })
}

/// The sync's lock, `<root>\update\suite.lock`: opened with no sharing (a link at the path is opened as itself),
/// held while the handle lives.
pub(crate) fn take_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
}

/// `run` under the sync's lock; another sync holding it: busy, exit 3, nothing done.
pub(crate) fn locked(lock: &Path, run: impl FnOnce() -> SyncResult) -> SyncResult {
    match take_lock(lock) {
        Ok(_held) => run(),
        Err(e) if e.raw_os_error() == Some(CODE_SHARING as i32) => {
            SyncResult::new(EXIT_RETRY, "busy", "another sync runs")
        }
        Err(e) => SyncResult::new(EXIT_RETRY, "unavailable", &format!("the sync's lock: {e}")),
    }
}

// ---------------------------------------------------------------------------------------------- U1e: the dock's side

/// The last result the dock read and the components as the install folder holds them, for `status`.
#[derive(Clone, Default)]
struct Known {
    line: Option<String>,
    failed: bool,
    components: Vec<KnownComponent>,
}

#[derive(Clone)]
struct KnownComponent {
    id: ComponentId,
    installed: bool,
    version: Option<String>,
    stage: Option<Stage>,
}

static KNOWN: std::sync::Mutex<Option<Known>> = std::sync::Mutex::new(None);

/// What the Suite page shows (F-P9): read on the UI thread, so it only copies what the update worker kept in memory
/// (`remember`); before the first sync of this start the components as the install folder held them at start.
#[allow(dead_code)] // the Suite page (P3-dock-suite) reads it
pub fn status() -> SuiteStatus {
    let k = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
    SuiteStatus {
        version: Version::current().to_string(),
        components: k
            .components
            .into_iter()
            .map(|c| ComponentStatus {
                id: c.id,
                installed: c.installed,
                version: c.version,
                stage: c.stage,
                enabled: true,
            })
            .collect(),
        last_sync: k.line.clone(),
        last_error: k.failed.then_some(k.line).flatten(),
    }
}

/// The line the dock logs for a finished `--suite-sync`: its result line when it printed one (one line of at most
/// RESULT_MAX bytes of text, no control character, starting with RESULT_PREFIX), else what is known.
pub(crate) fn result_line(code: i32, out: &[u8]) -> String {
    let first = out.split(|&b| b == b'\n').next().unwrap_or_default();
    let first = first.strip_suffix(b"\r").unwrap_or(first);
    match std::str::from_utf8(first) {
        Ok(l) if l.starts_with(RESULT_PREFIX) && l.len() <= RESULT_MAX && !l.chars().any(char::is_control) => {
            l.to_string()
        }
        _ => format!("{RESULT_PREFIX}error (exit {code} without a result line)"),
    }
}

/// A result line read: its state, whether it changed a component, and the components it left at another build.
pub(crate) struct ResultFacts {
    pub state: String,
    pub changed: bool,
    pub not_current: Vec<ComponentId>,
}

pub(crate) fn result_facts(line: &str) -> ResultFacts {
    let body = line.strip_prefix(RESULT_PREFIX).unwrap_or(line);
    let words = body.split(" (").next().unwrap_or_default();
    let mut tokens = words.split(' ');
    let state = tokens.next().unwrap_or_default().to_string();
    let (mut changed, mut not_current) = (false, Vec::new());
    for t in tokens {
        let Some((key, ids)) = t.split_once('=') else { continue };
        let ids = ids.split(',').filter_map(ComponentId::parse);
        match key {
            "added" | "replaced" => changed |= ids.count() > 0,
            "reverted" | "pending" => not_current.extend(ids),
            _ => {}
        }
    }
    ResultFacts { state, changed, not_current }
}

/// Keeps a sync's result line for `status` (the update worker, after each sync) and the components as the install
/// folder now holds them; true when the sync changed a component (the entry points follow, I1b).
pub(crate) fn remember(line: &str) -> bool {
    let facts = result_facts(line);
    let failed = !matches!(facts.state.as_str(), "ok" | "pending");
    let components = install_dir().map(|d| components_in(&d, &facts.not_current)).unwrap_or_default();
    *KNOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Known { line: Some(line.to_string()), failed, components });
    facts.changed
}

/// The components at the dock's start, before its first sync (the update-cleanup thread).
pub(crate) fn remember_installed() {
    let components = install_dir().map(|d| components_in(&d, &[])).unwrap_or_default();
    let mut k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if k.is_none() {
        *k = Some(Known { line: None, failed: false, components });
    }
}

fn install_dir() -> Option<PathBuf> {
    crate::autostart::installed_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// The components other than the dock as `dir` holds them: installed when the file is there; the version and stage
/// suite.json gives when it is this release's and the last sync did not leave the component at another build.
fn components_in(dir: &Path, not_current: &[ComponentId]) -> Vec<KnownComponent> {
    let m = oled_sys::datadir::read_file(&dir.join(SUITE_ASSET), SUITE_MAX_BYTES, false)
        .ok()
        .and_then(|b| parse_manifest(&b).ok())
        .filter(|m| m.version == Version::current());
    let exists = |id: ComponentId| std::fs::symlink_metadata(dir.join(id.file())).is_ok_and(|m| m.is_file());
    components_of(m.as_ref(), exists, not_current)
}

fn components_of(
    m: Option<&SuiteManifest>,
    exists: impl Fn(ComponentId) -> bool,
    not_current: &[ComponentId],
) -> Vec<KnownComponent> {
    ComponentId::ALL
        .into_iter()
        .filter(|c| *c != ComponentId::Dock)
        .map(|id| {
            let listed = m.and_then(|m| m.components.iter().find(|c| c.id == id).map(|c| (m.version, c.stage)));
            let installed = exists(id);
            let version = listed.filter(|_| installed && !not_current.contains(&id)).map(|(v, _)| v.to_string());
            KnownComponent { id, installed, version, stage: listed.map(|(_, s)| s) }
        })
        .collect()
}

/// The dock's `stage` of a newer release (update.rs): its suite.json and every component whose file differs from the
/// installed one (sizes and digests from the release, hashes from the manifest), so the sync after the update reads
/// the stage, not the network. A component already in place, or already staged, is not downloaded. Returns how many
/// files it staged; a release without a manifest stages nothing.
pub(crate) fn stage_components(
    rel: &Release,
    dir: &Path,
    net: &mut dyn SuiteNet,
    stage: &mut dyn SuiteStage,
) -> Result<u32, String> {
    let Some(asset) = &rel.suite else { return Ok(0) };
    let mut staged = 0;
    let have = stage.read(SUITE_ASSET, SUITE_MAX_BYTES).ok().filter(|b| {
        b.len() as u64 == asset.size && asset.digest.is_none_or(|d| oled_sys::sha256::sha256(b).is_ok_and(|h| h == d))
    });
    let bytes = match have {
        Some(b) => b,
        None => {
            let b = fetch_checked(net, asset, SUITE_MAX_BYTES, None).map_err(|e| format!("suite.json: {e}"))?;
            stage.write(SUITE_ASSET, &b).map_err(|e| format!("suite.json: {e}"))?;
            staged += 1;
            b
        }
    };
    let m = parse_manifest(&bytes)?;
    if m.version != rel.version {
        return Err("suite.json is of another version".into());
    }
    let mut failed = Vec::new();
    for c in m.components.iter().filter(|c| c.id != ComponentId::Dock) {
        let in_place = hash_file(&dir.join(c.id.file())).ok().flatten().is_some_and(|(h, _)| h == c.sha256);
        let ready = stage
            .read(c.id.asset(), COMPONENT_MAX_BYTES)
            .ok()
            .and_then(|b| oled_sys::sha256::sha256(&b).ok())
            .is_some_and(|h| h == c.sha256);
        if in_place || ready {
            continue;
        }
        let got = rel
            .component_assets
            .iter()
            .find(|(id, _)| *id == c.id)
            .map(|(_, a)| a)
            .ok_or_else(|| "not among the release's assets".to_string())
            .and_then(|a| {
                if a.size == c.size {
                    fetch_checked(net, a, COMPONENT_MAX_BYTES, Some(c.sha256))
                } else {
                    Err("the asset is not the size suite.json gives".into())
                }
            })
            .and_then(|b| stage.write(c.id.asset(), &b));
        match got {
            Ok(()) => staged += 1,
            Err(e) => failed.push(format!("{}: {e}", c.id.id())),
        }
    }
    if failed.is_empty() {
        Ok(staged)
    } else {
        Err(failed.join("; "))
    }
}

/// true while the stage of the running version holds a component that is not in the install folder yet, or the
/// strikes of a build that is not (they keep a failing build from being tried at every start): the start's cleanup
/// keeps it for the sync (which removes it after an "ok").
pub(crate) fn stage_holds_new_components(stage: &mut dyn SuiteStage, dir: &Path) -> bool {
    ComponentId::ALL.into_iter().filter(|c| *c != ComponentId::Dock).any(|id| {
        let staged = stage.read(id.asset(), COMPONENT_MAX_BYTES).ok().and_then(|b| oled_sys::sha256::sha256(&b).ok());
        let struck = stage.read(&strike_name(id), STRIKE_MAX_BYTES).ok().and_then(|b| {
            let text = String::from_utf8_lossy(&b).into_owned();
            text.split_whitespace().next().and_then(parse_hex)
        });
        if staged.is_none() && struck.is_none() {
            return false;
        }
        let installed = hash_file(&dir.join(id.file())).ok().flatten().map(|(h, _)| h);
        [staged, struck].into_iter().flatten().any(|h| installed != Some(h))
    })
}

/// 64 hex digits as a SHA-256 (a strike file's first word).
fn parse_hex(s: &str) -> Option<Digest> {
    let mut out = [0u8; 32];
    if s.len() != 64 {
        return None;
    }
    for (o, i) in out.iter_mut().zip((0..64).step_by(2)) {
        *o = u8::from_str_radix(s.get(i..i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// `stage_holds_new_components` of the running version's stage, from the dock's start (update.rs cleanup_stages).
pub(crate) fn own_stage_pending(tag: &str) -> bool {
    install_dir().is_some_and(|dir| stage_holds_new_components(&mut DataStage { tag: tag.to_string() }, &dir))
}

#[cfg(test)]
mod tests {
    //! U1s: the swap measured over a running image. The running image is this test exe, copied into a temp folder and
    //! started on `sleep_entry` alone with `OLED_SLEEP_TEST` set; it is killed when the test ends.
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH, REPLACEFILE_IGNORE_MERGE_ERRORS,
    };
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;

    const SLEEP_ENV: &str = "OLED_SLEEP_TEST";
    const NEW_BYTES: &[u8] = b"U1s: the bytes of the new image";

    /// The running image of the swap tests: started with `OLED_SLEEP_TEST=<seconds>` it sleeps; otherwise it returns.
    #[test]
    #[ignore = "the running image of the U1s swap tests (OLED_SLEEP_TEST=<seconds>)"]
    fn sleep_entry() {
        if let Some(s) = std::env::var(SLEEP_ENV).ok().and_then(|v| v.parse::<u64>().ok()) {
            std::thread::sleep(Duration::from_secs(s.min(60)));
        }
    }

    /// A temp folder with a random name, removed with what it holds when dropped.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            // The clock ticks in 100 ns on Windows: two tests may start in one tick, so a counter keeps names apart.
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!("dock-swap-{tag}-{}-{n}-{nanos:x}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create the temp folder");
            TempDir(dir)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A copy of this test exe running `sleep_entry`; killed and waited for when dropped, so a failing test leaves no
    /// process behind.
    struct Running(Child);
    impl Running {
        fn start(exe: &Path) -> Running {
            let child = Command::new(exe)
                .creation_flags(CREATE_NO_WINDOW.0)
                .args(["--ignored", "--exact", "update::suite::tests::sleep_entry", "--test-threads=1", "-q"])
                .env(SLEEP_ENV, "10")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start the running image");
            std::thread::sleep(Duration::from_millis(300)); // past the loader: the image is mapped and its thread runs
            Running(child)
        }
        fn alive(&mut self) -> bool {
            matches!(self.0.try_wait(), Ok(None))
        }
    }
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// `run.exe` (a copy of this test exe) in a new temp folder, and the path of the original.
    fn image_in(dir: &TempDir) -> (PathBuf, PathBuf) {
        let me = std::env::current_exe().expect("the test exe");
        let run = dir.path("run.exe");
        std::fs::copy(&me, &run).expect("copy the test exe");
        (me, run)
    }

    /// Byte for byte, read in chunks (the test exe is large).
    fn same_bytes(a: &Path, b: &Path) -> bool {
        let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else { return false };
        let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
        loop {
            let (Ok(na), Ok(nb)) = (fa.read(&mut ba), fb.read(&mut bb)) else { return false };
            if na != nb || ba.get(..na) != bb.get(..nb) {
                return false;
            }
            if na == 0 {
                return true;
            }
        }
    }

    fn replace(dest: &Path, new: &Path, backup: Option<&Path>) -> windows::core::Result<()> {
        let (dest, new) = (HSTRING::from(dest), HSTRING::from(new));
        let backup = backup.map(HSTRING::from);
        let backup = backup.as_ref().map_or(PCWSTR::null(), |b| PCWSTR(b.as_ptr()));
        // SAFETY: three NUL-terminated paths (or a null backup name) that outlive the call; no exclude or reserved data.
        unsafe { ReplaceFileW(&dest, &new, backup, REPLACEFILE_IGNORE_MERGE_ERRORS, None, None) }
    }

    fn rename(from: &Path, to: &Path) -> windows::core::Result<()> {
        let (from, to) = (HSTRING::from(from), HSTRING::from(to));
        // SAFETY: two NUL-terminated paths that outlive the call.
        unsafe { MoveFileExW(&from, &to, MOVEFILE_WRITE_THROUGH) }
    }

    #[test]
    fn replace_with_backup_over_a_running_image() {
        let dir = TempDir::new("replace-backup");
        let (me, run) = image_in(&dir);
        let (new, old) = (dir.path("run.exe.new"), dir.path("run.exe.old-1"));
        let mut image = Running::start(&run);
        std::fs::write(&new, NEW_BYTES).expect("write run.exe.new");
        let got = replace(&run, &new, Some(&old));
        eprintln!("U1s: ReplaceFileW(run.exe, run.exe.new, run.exe.old-1) over a running image: {got:?}");
        if got.is_ok() {
            assert_eq!(std::fs::read(&run).ok().as_deref(), Some(NEW_BYTES), "run.exe holds the new bytes");
            assert!(same_bytes(&old, &me), "run.exe.old-1 holds the old image");
            assert!(image.alive(), "the old image keeps running");
        }
        assert!(
            SWAP != Swap::Replace || got.is_ok(),
            "SWAP is Replace, so ReplaceFileW with a backup name must work here"
        );
    }

    #[test]
    fn replace_without_backup_over_a_running_image() {
        let dir = TempDir::new("replace-nobackup");
        let (me, run) = image_in(&dir);
        let new = dir.path("run.exe.new");
        let mut image = Running::start(&run);
        std::fs::write(&new, NEW_BYTES).expect("write run.exe.new");
        let got = replace(&run, &new, None);
        let holds = if std::fs::read(&run).ok().as_deref() == Some(NEW_BYTES) {
            "the new bytes"
        } else if same_bytes(&run, &me) {
            "the old image"
        } else {
            "neither (missing or other)"
        };
        let mut names: Vec<String> = std::fs::read_dir(&dir.0)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        // Recorded only: never used by the swap.
        eprintln!(
            "U1s: ReplaceFileW(run.exe, run.exe.new, null) over a running image: {got:?}; run.exe holds {holds}; \
             the image runs: {}; the folder holds {names:?}",
            image.alive()
        );
    }

    #[test]
    fn rename_pair_over_a_running_image() {
        let dir = TempDir::new("rename-pair");
        let (me, run) = image_in(&dir);
        let (new, old) = (dir.path("run.exe.new"), dir.path("run.exe.old-2"));
        let mut image = Running::start(&run);
        std::fs::write(&new, NEW_BYTES).expect("write run.exe.new");
        let first = rename(&run, &old);
        let second = if first.is_ok() { rename(&new, &run) } else { Ok(()) };
        eprintln!("U1s: MoveFileExW pair over a running image: {first:?}, then {second:?}");
        assert!(first.is_ok() && second.is_ok(), "the rename pair works over a running image");
        assert_eq!(std::fs::read(&run).ok().as_deref(), Some(NEW_BYTES), "run.exe holds the new bytes");
        assert!(same_bytes(&old, &me), "run.exe.old-2 holds the old image");
        assert!(image.alive(), "the old image keeps running");
    }

    #[test]
    fn replace_with_backup_over_an_idle_file() {
        let dir = TempDir::new("replace-idle");
        let (run, new, old) = (dir.path("run.exe"), dir.path("run.exe.new"), dir.path("run.exe.old-1"));
        std::fs::write(&run, b"the old bytes").expect("write run.exe");
        std::fs::write(&new, NEW_BYTES).expect("write run.exe.new");
        let got = replace(&run, &new, Some(&old));
        eprintln!("U1s: ReplaceFileW with a backup name over an idle file: {got:?}");
        assert!(got.is_ok(), "{got:?}");
        assert_eq!(std::fs::read(&run).ok().as_deref(), Some(NEW_BYTES));
        assert_eq!(std::fs::read(&old).ok().as_deref(), Some(&b"the old bytes"[..]));
        assert!(!new.exists(), "the replacement took the name");
    }

    // ------------------------------------------------------------------------------------------------ U1c: the swap

    use super::*;
    use oled_sys::journal_os::{open_locked_at, FileSink};
    use oled_trust::journal::{Entry, Record, Scope, Target};
    use oled_trust::manifest::release::Version;
    use std::collections::BTreeMap;

    const LOCK: Duration = Duration::from_secs(5);
    const OLD: &[u8] = b"tasks.exe as 1.7.0 shipped it";
    const PREV: &[u8] = b"tasks.exe as 1.6.0 shipped it";
    const NEW: &[u8] = b"tasks.exe as 1.8.0 ships it";
    const LANG: &[u8] = b"OLNG it 1.8.0 (a made-up language file)";
    const DOCK: &[u8] = b"the dock, which no swap touches";

    /// The test side of a swap: where it stops (as a crash would), a file it changes at a step (as another program
    /// would), and the deletes it asks for at the next restart (never sent to Windows here).
    #[derive(Default)]
    struct TestEnv {
        stop_after: Option<u8>,
        tamper: Option<(u8, PathBuf, Vec<u8>)>,
        steps: Vec<u8>,
        at_restart: Vec<PathBuf>,
    }
    impl SwapEnv for TestEnv {
        fn go_on(&mut self, step: u8) -> bool {
            self.steps.push(step);
            if let Some((at, path, bytes)) = &self.tamper {
                if *at == step {
                    std::fs::write(path, bytes).expect("tamper");
                }
            }
            self.stop_after != Some(step)
        }
        fn delete_at_restart(&mut self, path: &Path) -> Result<(), String> {
            self.at_restart.push(path.to_path_buf());
            Ok(())
        }
    }

    fn component(id: ComponentId, bytes: &[u8]) -> Component {
        let sha256 = oled_sys::sha256::sha256(bytes).expect("sha256");
        Component { id, version: Version::current(), sha256, size: bytes.len() as u64, stage: Stage::Beta }
    }

    /// A temp root holding the install folder, a stage folder and update.jnl (outside the install folder, as
    /// <root>\update.jnl is).
    struct Suite {
        root: TempDir,
        inst: PathBuf,
        stage: PathBuf,
        jnl: PathBuf,
    }
    impl Suite {
        fn new(tag: &str) -> Suite {
            let root = TempDir::new(tag);
            let (inst, stage, jnl) = (root.path("inst"), root.path("stage"), root.path("update.jnl"));
            std::fs::create_dir_all(&inst).expect("inst");
            std::fs::create_dir_all(&stage).expect("stage");
            let s = Suite { root, inst, stage, jnl };
            s.put("dock.exe", DOCK);
            s
        }
        fn put(&self, rel: &str, bytes: &[u8]) {
            let p = self.inst.join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("folder");
            std::fs::write(p, bytes).expect("write");
        }
        fn staged(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.stage.join(name);
            std::fs::write(&p, bytes).expect("stage");
            p
        }
        fn read(&self, rel: &str) -> Option<Vec<u8>> {
            std::fs::read(self.inst.join(rel)).ok()
        }
        fn open(&self) -> (Journal, FileSink) {
            open_locked_at(&self.jnl, Scope::Machine, LOCK).expect("update.jnl")
        }
        fn records(&self) -> Vec<Record> {
            let bytes = std::fs::read(&self.jnl).unwrap_or_default();
            Journal::parse(Scope::Machine, &bytes).expect("parse").records().to_vec()
        }
        fn path(&self, rel: &str) -> String {
            self.inst.join(rel).to_str().expect("unicode").to_string()
        }
    }

    /// Every file and folder under `dir` by its install-relative name (folders end with a backslash), with the bytes.
    fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).expect("read_dir").flatten() {
                let name = format!("{rel}{}", e.file_name().to_string_lossy());
                if e.file_type().expect("type").is_dir() {
                    out.insert(format!("{name}\\"), Vec::new());
                    walk(&e.path(), &format!("{name}\\"), out);
                } else {
                    out.insert(name, std::fs::read(e.path()).expect("read"));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, "", &mut out);
        out
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Case {
        /// tasks.exe is new.
        Add,
        /// tasks.exe 1.7.0 -> 1.8.0, no previous copy yet.
        Replace,
        /// tasks.exe 1.7.0 -> 1.8.0 with a 1.6.0 previous copy.
        ReplaceWithPrev,
        /// back to the previous copy (a rollback's sync): tasks.exe 1.7.0, tasks.prev.exe 1.8.0.
        FromPrev,
        /// lang\it.lang is new, and so is lang\.
        LangAdd,
    }
    const CASES: [Case; 5] = [Case::Add, Case::Replace, Case::ReplaceWithPrev, Case::FromPrev, Case::LangAdd];

    fn setup(case: Case) -> (Suite, Component, PathBuf) {
        let s = Suite::new(&format!("{case:?}").to_lowercase());
        let (comp, staged) = match case {
            Case::Add => (component(ComponentId::Tasks, NEW), s.staged("tasks.exe", NEW)),
            Case::Replace => {
                s.put("tasks.exe", OLD);
                (component(ComponentId::Tasks, NEW), s.staged("tasks.exe", NEW))
            }
            Case::ReplaceWithPrev => {
                s.put("tasks.exe", OLD);
                s.put("tasks.prev.exe", PREV);
                (component(ComponentId::Tasks, NEW), s.staged("tasks.exe", NEW))
            }
            Case::FromPrev => {
                s.put("tasks.exe", OLD);
                s.put("tasks.prev.exe", NEW);
                (component(ComponentId::Tasks, NEW), s.inst.join("tasks.prev.exe"))
            }
            Case::LangAdd => (component(ComponentId::LangIt, LANG), s.staged("lang-it.lang", LANG)),
        };
        (s, comp, staged)
    }

    /// Every name under `inst` that is not in `before` is one the swap may make (its names or their .old copies).
    fn assert_only_swap_names(inst: &Path, before: &BTreeMap<String, Vec<u8>>, what: &str) {
        for name in snapshot(inst).keys().filter(|n| !before.contains_key(*n) && !n.ends_with('\\')) {
            assert!(is_swap_path(inst, &inst.join(name)), "{what}: {name} is not a swap name");
        }
    }

    #[test]
    fn add_component() {
        let (s, comp, staged) = setup(Case::Add);
        // Stopped after step 2: update.jnl holds the swap, flushed, and nothing has been copied yet.
        {
            let (mut j, mut sink) = s.open();
            let mut env = TestEnv { stop_after: Some(2), ..TestEnv::default() };
            let got = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env);
            assert!(matches!(got, Err(SwapError::Stopped { run: Some(_), step: 2 })), "{got:?}");
        }
        let want = Change::FileSwap { dest: s.path("tasks.exe"), prev: None, staged_sha256: comp.sha256 };
        assert!(matches!(&s.records()[..], [Record::Entry(e)] if e.change == want && e.kind == Kind::Update));
        assert_eq!((s.read("tasks.exe"), s.read("tasks.exe.new")), (None, None));
        assert!(replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay").deleted);
        // The whole swap: the file is in place, its hash checked again, nothing else left.
        let (mut j, mut sink) = s.open();
        let mut env = TestEnv::default();
        let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
        assert_eq!(env.steps, [1, 2, 3, 4, 5, 6]);
        assert_eq!(s.read("tasks.exe").as_deref(), Some(NEW));
        assert_eq!(s.read("tasks.prev.exe"), None, "an addition makes no previous copy");
        assert_eq!(snapshot(&s.inst).keys().cloned().collect::<Vec<_>>(), ["dock.exe", "tasks.exe"]);
        assert!(j.open_entries().iter().all(|e| e.run == done.run));
        // A staged file that is not the manifest's is refused before anything is recorded.
        let wrong = s.staged("files.exe", b"not the release's files.exe");
        let files = component(ComponentId::Files, b"files.exe 1.8.0");
        let got = swap_in(&s.inst, &files, &wrong, &mut j, &mut sink, &mut TestEnv::default());
        assert!(matches!(got, Err(SwapError::Staged(_))), "{got:?}");
        assert_eq!(s.read("files.exe"), None);
        assert_eq!(j.open_entries().len(), 1, "nothing recorded for the refused file");
    }

    #[test]
    fn replace_component() {
        let (s, comp, staged) = setup(Case::Replace);
        let (mut j, mut sink) = s.open();
        let mut env = TestEnv::default();
        let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
        assert_eq!(s.read("tasks.exe").as_deref(), Some(NEW));
        assert_eq!(s.read("tasks.prev.exe").as_deref(), Some(OLD), "the old file is the previous copy");
        assert_eq!(snapshot(&s.inst).keys().cloned().collect::<Vec<_>>(), ["dock.exe", "tasks.exe", "tasks.prev.exe"]);
        assert!(env.at_restart.is_empty(), "an idle replaced file is deleted at once");
        let old = oled_sys::sha256::sha256(OLD).expect("sha256");
        let changes: Vec<Change> = j.open_entries().iter().map(|e| e.change.clone()).collect();
        assert_eq!(
            changes,
            [
                Change::FileSwap { dest: s.path("tasks.prev.exe"), prev: None, staged_sha256: old },
                Change::FileSwap {
                    dest: s.path("tasks.exe"),
                    prev: Some(s.path("tasks.prev.exe")),
                    staged_sha256: comp.sha256
                },
            ]
        );
        assert!(done.prev_kept.is_none());
        // With an earlier previous copy, it is kept aside until the commit, then deleted.
        let (s, comp, staged) = setup(Case::ReplaceWithPrev);
        let (mut j, mut sink) = s.open();
        let mut env = TestEnv::default();
        let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
        assert_eq!((s.read("tasks.exe").as_deref(), s.read("tasks.prev.exe").as_deref()), (Some(NEW), Some(OLD)));
        let kept = done.prev_kept.clone().expect("the 1.6.0 copy is kept until the commit");
        assert_eq!(std::fs::read(&kept).ok().as_deref(), Some(PREV));
        commit_swap(&mut j, &mut sink, done, &mut env).expect("commit");
        assert!(!kept.exists(), "deleted at the commit");
        assert_eq!(snapshot(&s.inst).keys().cloned().collect::<Vec<_>>(), ["dock.exe", "tasks.exe", "tasks.prev.exe"]);
    }

    #[test]
    fn from_prev() {
        let (s, comp, staged) = setup(Case::FromPrev);
        let (mut j, mut sink) = s.open();
        let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut TestEnv::default()).expect("swap");
        assert_eq!(s.read("tasks.exe").as_deref(), Some(NEW), "the previous copy is back in place");
        assert_eq!(s.read("tasks.prev.exe").as_deref(), Some(OLD), "the old current file is the new previous copy");
        commit_swap(&mut j, &mut sink, done, &mut TestEnv::default()).expect("commit");
        assert_eq!(snapshot(&s.inst).keys().cloned().collect::<Vec<_>>(), ["dock.exe", "tasks.exe", "tasks.prev.exe"]);
    }

    #[test]
    fn revert_ownership() {
        // Reverted as it is: the old file is back, the previous copy made by the swap is gone.
        let (s, comp, staged) = setup(Case::Replace);
        let before = snapshot(&s.inst);
        let (mut j, mut sink) = s.open();
        let mut env = TestEnv::default();
        let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
        let r = revert(&s.inst, done.id, done.run, &mut j, &mut sink, &mut env);
        assert_eq!((r.restored, r.failed.len(), r.changed_by_other.len()), (2, 0, 0), "{r:?}");
        assert_eq!(snapshot(&s.inst), before);
        assert!(j.is_settled());
        // Changed by another program after our swap: that file is left as it is and reported.
        let (s, comp, staged) = setup(Case::Replace);
        let (mut j, mut sink) = s.open();
        let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
        s.put("tasks.exe", b"someone else's tasks.exe");
        let r = revert(&s.inst, done.id, done.run, &mut j, &mut sink, &mut env);
        assert_eq!(r.changed_by_other, [Target::File(s.path("tasks.exe"))]);
        assert_eq!(s.read("tasks.exe").as_deref(), Some(&b"someone else's tasks.exe"[..]));
        let prev = s.path("tasks.prev.exe");
        let open_prev = |e: &&Entry| matches!(&e.change, Change::FileSwap { dest, .. } if *dest == prev);
        assert!(j.open_entries().iter().all(open_prev), "closed: the file is no longer ours");
        // The previous copy holds the only copy left of the file the run replaced: it stays
        // (the_previous_copy_outlives_another_programs_file).
        assert_eq!(s.read("tasks.prev.exe").as_deref(), Some(OLD));
    }

    #[test]
    fn commit_closes_the_run() {
        let (s, comp, staged) = setup(Case::Replace);
        let (mut j, mut sink) = s.open();
        let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut TestEnv::default()).expect("swap");
        let run = done.run;
        assert!(!j.is_settled());
        commit_swap(&mut j, &mut sink, done, &mut TestEnv::default()).expect("commit");
        assert!(j.open_entries().iter().all(|e| e.run != run) && j.is_settled());
        drop((j, sink));
        assert!(s.records().contains(&Record::Committed { run }), "on disk");
        // A committed run is never reverted.
        let (mut j, mut sink) = s.open();
        let r = revert(&s.inst, comp.id, run, &mut j, &mut sink, &mut TestEnv::default());
        assert_eq!(r, UndoReport::default());
        assert_eq!(s.read("tasks.exe").as_deref(), Some(NEW));
    }

    #[test]
    fn a_successful_sync_then_replay_changes_nothing() {
        for case in CASES {
            let (s, comp, staged) = setup(case);
            {
                let (mut j, mut sink) = s.open();
                let mut env = TestEnv::default();
                let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
                // The selftest passed (U1d; a fake here), so the run is committed.
                commit_swap(&mut j, &mut sink, done, &mut env).expect("commit");
            }
            let after = snapshot(&s.inst);
            let r = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
            assert_eq!(snapshot(&s.inst), after, "{case:?}: no byte changed");
            assert!(r.deleted && !s.jnl.exists(), "{case:?}: update.jnl deleted");
            assert_eq!(r.runs, 0, "{case:?}");
        }
    }

    #[test]
    fn crash_between_selftest_and_commit() {
        for case in CASES {
            let (s, comp, staged) = setup(case);
            let before = snapshot(&s.inst);
            {
                let (mut j, mut sink) = s.open();
                let _done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut TestEnv::default()).expect("swap");
                // The selftest passed, then the process ended before Committed was appended.
            }
            let r = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
            assert_eq!(snapshot(&s.inst), before, "{case:?}: back to the state before the swap");
            assert!(r.deleted && !s.jnl.exists(), "{case:?}");
            assert_eq!((r.runs, r.report.failed.len()), (1, 0), "{case:?}");
            assert!(staged.exists(), "{case:?}: the stage still holds the file, so the next sync adds it again");
        }
    }

    #[test]
    fn replay_after_a_crash_at_every_step() {
        for case in CASES {
            for step in 1..=6 {
                let (s, comp, staged) = setup(case);
                let before = snapshot(&s.inst);
                {
                    let (mut j, mut sink) = s.open();
                    let mut env = TestEnv { stop_after: Some(step), ..TestEnv::default() };
                    let got = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env);
                    assert!(
                        matches!(got, Err(SwapError::Stopped { step: k, .. }) if k == step),
                        "{case:?} {step}: {got:?}"
                    );
                }
                assert_only_swap_names(&s.inst, &before, &format!("{case:?} after step {step}"));
                let r = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
                assert_eq!(snapshot(&s.inst), before, "{case:?}, stopped after step {step}");
                assert!(r.deleted && !s.jnl.exists(), "{case:?} {step}: update.jnl deleted");
                assert!(r.report.failed.is_empty(), "{case:?} {step}: {:?}", r.report);
            }
        }
    }

    #[test]
    fn the_hash_is_checked_again_after_the_swap() {
        for case in [Case::Add, Case::Replace, Case::ReplaceWithPrev, Case::FromPrev] {
            let (s, comp, staged) = setup(case);
            let before = snapshot(&s.inst);
            let (mut j, mut sink) = s.open();
            // Another program writes tasks.exe.new between the copy's check and the swap.
            let tamper = Some((3, s.inst.join("tasks.exe.new"), b"not the release's tasks.exe".to_vec()));
            let mut env = TestEnv { tamper, ..TestEnv::default() };
            let got = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env);
            let Err(SwapError::Failed { run: Some(run), .. }) = got else { panic!("{case:?}: {got:?}") };
            assert_eq!(env.steps, [1, 2, 3, 4, 5], "{case:?}: refused at step 6");
            // The swap put those bytes at tasks.exe itself, so it took them back at once: the old file survives.
            assert_eq!(s.read("tasks.exe").as_deref(), before.get("tasks.exe").map(Vec::as_slice), "{case:?}");
            let r = revert(&s.inst, comp.id, run, &mut j, &mut sink, &mut env);
            assert!(r.failed.is_empty() && r.changed_by_other.is_empty(), "{case:?}: {r:?}");
            assert_eq!(snapshot(&s.inst), before, "{case:?}");
            assert!(j.is_settled(), "{case:?}");
        }
    }

    /// The cases where the swap moves the file it replaces into the previous copy.
    const WITH_PREV: [Case; 3] = [Case::Replace, Case::ReplaceWithPrev, Case::FromPrev];

    #[test]
    fn a_quarantined_component_is_put_back() {
        for case in WITH_PREV {
            // The new tasks.exe is gone after the swap (an AV quarantine) and its selftest failed: the revert puts the
            // file the run replaced back from the previous copy, then the previous copy as it was.
            let (s, comp, staged) = setup(case);
            let before = snapshot(&s.inst);
            let (mut j, mut sink) = s.open();
            let mut env = TestEnv::default();
            let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
            std::fs::remove_file(s.inst.join("tasks.exe")).expect("quarantine");
            let r = revert(&s.inst, done.id, done.run, &mut j, &mut sink, &mut env);
            assert!(r.failed.is_empty() && r.changed_by_other.is_empty(), "{case:?}: {r:?}");
            assert_eq!(snapshot(&s.inst), before, "{case:?}: reverted");
            assert!(j.is_settled(), "{case:?}");
            // The same when the process ended before its revert: the next sync's replay does it.
            let (s, comp, staged) = setup(case);
            let before = snapshot(&s.inst);
            {
                let (mut j, mut sink) = s.open();
                let _done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut TestEnv::default()).expect("swap");
            }
            std::fs::remove_file(s.inst.join("tasks.exe")).expect("quarantine");
            let r = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
            assert_eq!(snapshot(&s.inst), before, "{case:?}: replayed");
            assert!(r.deleted && r.report.failed.is_empty(), "{case:?}: {:?}", r.report);
        }
    }

    #[test]
    fn the_previous_copy_outlives_another_programs_file() {
        let foreign: &[u8] = b"someone else's tasks.exe";
        for case in WITH_PREV {
            let (s, comp, staged) = setup(case);
            let before = snapshot(&s.inst);
            {
                let (mut j, mut sink) = s.open();
                let mut env = TestEnv::default();
                let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
                // Another program writes tasks.exe, then the selftest fails: that file is no longer ours and is left,
                // and the previous copy, the only copy of the file the run replaced, stays with its entry open.
                s.put("tasks.exe", foreign);
                let r = revert(&s.inst, done.id, done.run, &mut j, &mut sink, &mut env);
                assert_eq!(r.changed_by_other, [Target::File(s.path("tasks.exe"))], "{case:?}");
                let failed: Vec<&Target> = r.failed.iter().map(|(t, _)| t).collect();
                assert_eq!(failed, [&Target::File(s.path("tasks.prev.exe"))], "{case:?}");
                assert_eq!(s.read("tasks.exe").as_deref(), Some(foreign), "{case:?}");
                assert_eq!(s.read("tasks.prev.exe").as_deref(), Some(OLD), "{case:?}: the old file survives");
            }
            // Every sync's replay tries again, and keeps the copy while the name holds other bytes...
            let r = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
            assert_eq!((s.read("tasks.prev.exe").as_deref(), r.deleted), (Some(OLD), false), "{case:?}");
            // ...then finishes the revert once the old file is back at its name.
            s.put("tasks.exe", OLD);
            let r = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
            assert_eq!(snapshot(&s.inst), before, "{case:?}");
            assert!(r.deleted && r.report.failed.is_empty(), "{case:?}: {:?}", r.report);
        }
    }

    #[test]
    fn a_blocked_file_keeps_its_previous_copy() {
        use std::os::windows::fs::OpenOptionsExt;
        let (s, comp, staged) = setup(Case::Replace);
        let before = snapshot(&s.inst);
        {
            let (mut j, mut sink) = s.open();
            let mut env = TestEnv::default();
            let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap");
            // A scanner holds tasks.exe open with no sharing while the revert runs: it can be neither read nor replaced.
            let held =
                std::fs::OpenOptions::new().read(true).share_mode(0).open(s.inst.join("tasks.exe")).expect("hold");
            let r = revert(&s.inst, done.id, done.run, &mut j, &mut sink, &mut env);
            let failed: Vec<&Target> = r.failed.iter().map(|(t, _)| t).collect();
            assert_eq!(failed, [&Target::File(s.path("tasks.exe")), &Target::File(s.path("tasks.prev.exe"))], "{r:?}");
            assert_eq!(s.read("tasks.prev.exe").as_deref(), Some(OLD), "the old file survives");
            drop(held);
        }
        // Once the scan is over, the next sync's replay finishes the revert.
        let r = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
        assert_eq!(snapshot(&s.inst), before);
        assert!(r.deleted && r.report.failed.is_empty(), "{:?}", r.report);
    }

    #[test]
    fn open_runs_are_undone_newest_first_once_each() {
        let ids = |v: &[u64]| v.iter().map(|&n| RunId(n)).collect::<Vec<_>>();
        assert_eq!(newest_first(ids(&[3, 1, 3, 2, 1])), ids(&[3, 2, 1]));
        assert_eq!(newest_first(ids(&[1, 2, 3])), ids(&[3, 2, 1]));
        assert_eq!(newest_first(ids(&[9, 4, 4, 9])), ids(&[9, 4]));
        assert_eq!(newest_first(ids(&[7])), ids(&[7]));
        assert!(newest_first(ids(&[])).is_empty());
    }

    #[test]
    fn replay_is_idempotent() {
        let (s, comp, staged) = setup(Case::ReplaceWithPrev);
        let before = snapshot(&s.inst);
        {
            let (mut j, mut sink) = s.open();
            let mut env = TestEnv { stop_after: Some(5), ..TestEnv::default() };
            assert!(swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).is_err());
        }
        let crashed = std::fs::read(&s.jnl).expect("update.jnl");
        let first = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
        assert_eq!(snapshot(&s.inst), before);
        assert!(first.report.restored >= 1, "{:?}", first.report);
        // As if the first replay's Undone lines were lost: a second one finds everything in its before state.
        std::fs::write(&s.jnl, &crashed).expect("put update.jnl back");
        let second = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
        assert_eq!(snapshot(&s.inst), before);
        assert_eq!(second.report.restored, 0, "{:?}", second.report);
        assert!(second.report.failed.is_empty() && second.deleted);
        let third = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
        assert_eq!((third.runs, snapshot(&s.inst) == before), (0, true));
    }

    #[test]
    fn swap_names_are_removed() {
        // FC1's assertion over the real removal list: every name a swap makes, and its .old-<n> copies.
        let dir = Path::new(r"C:\P\m");
        for id in ComponentId::ALL.into_iter().filter(|c| *c != ComponentId::Dock) {
            crate::autostart::assert_uninstall_removes(dir, &swap_names(id));
        }
        // ...and the swap really makes nothing else (checked at every step for every case in
        // replay_after_a_crash_at_every_step; here the .old names of a replace over a running image).
        assert!(is_swap_path(dir, &dir.join("tasks.exe.old-12")));
        assert!(is_swap_path(dir, &dir.join(r"lang\it.prev.lang.new")));
        for other in [
            "dock.exe",
            "dock.exe.new",
            "dock.prev.exe",
            "dock.exe.old-1",
            "tasks.exe.old-",
            "tasks.exe.old-1x",
            r"lang\..\dock.exe",
            "tasks.exe.restore",
            r"sub\tasks.exe",
        ] {
            assert!(!is_swap_path(dir, &dir.join(other)), "{other}");
        }
        assert!(!is_swap_path(dir, Path::new(r"C:\P\other\tasks.exe")));
        assert!(is_swap_path(dir, Path::new(r"c:\p\M\TASKS.EXE")), "case does not matter on Windows");
    }

    #[test]
    fn forged_entries_touch_nothing_but_component_files() {
        let s = Suite::new("forged");
        let outside = s.root.path("other.txt");
        std::fs::write(&outside, b"not ours").expect("write");
        {
            let (mut j, mut sink) = s.open();
            let run = j.begin_run();
            let dock = oled_sys::sha256::sha256(DOCK).expect("sha256");
            let other = oled_sys::sha256::sha256(b"not ours").expect("sha256");
            let outside_text = outside.to_str().expect("unicode").to_string();
            for (dest, staged) in [(s.path("dock.exe"), dock), (outside_text, other)] {
                let change = Change::FileSwap { dest, prev: None, staged_sha256: staged };
                journal::record(&mut j, &mut sink, run, Kind::Update, change, 1).expect("record");
            }
        }
        let r = replay_open(&s.inst, &s.jnl, &mut TestEnv::default()).expect("replay");
        assert_eq!(s.read("dock.exe").as_deref(), Some(DOCK), "the dock image is never touched");
        assert_eq!(std::fs::read(&outside).ok().as_deref(), Some(&b"not ours"[..]));
        assert_eq!((r.report.failed.len(), r.deleted), (2, false), "{:?}", r.report);
    }

    #[test]
    fn the_dock_is_never_swapped() {
        let s = Suite::new("dock");
        let comp = component(ComponentId::Dock, b"another dock");
        let staged = s.staged("dock.exe", b"another dock");
        let (mut j, mut sink) = s.open();
        let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut TestEnv::default())
        }));
        // A debug build stops at the debug_assert; a release build refuses.
        assert!(matches!(got, Err(_) | Ok(Err(SwapError::Dock))), "{got:?}");
        assert_eq!(s.read("dock.exe").as_deref(), Some(DOCK));
        assert!(j.records().is_empty());
    }

    #[test]
    fn lang_file_creates_its_folder() {
        use windows::Win32::Security::SECURITY_ATTRIBUTES;
        use windows::Win32::Storage::FileSystem::{CreateDirectoryW, READ_CONTROL};
        let s = Suite::new("lang");
        // An install folder with a DACL of its own (protected, inheritable), as Program Files gives one.
        let user = oled_sys::pipe::current_user_sid().expect("user");
        let inst = s.root.path("acl");
        let sddl = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{user})");
        let sd = oled_sys::datadir::SecDesc::from_sddl(&sddl).expect("sddl");
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.as_ptr().0,
            bInheritHandle: false.into(),
        };
        // SAFETY: a NUL-terminated path; `sa` and the descriptor it points to live for the call.
        unsafe { CreateDirectoryW(&HSTRING::from(inst.as_os_str()), Some(&sa)) }.expect("install folder");
        let comp = component(ComponentId::LangIt, LANG);
        let staged = s.staged("lang-it.lang", LANG);
        let (mut j, mut sink) = s.open();
        swap_in(&inst, &comp, &staged, &mut j, &mut sink, &mut TestEnv::default()).expect("swap");
        assert_eq!(std::fs::read(inst.join(r"lang\it.lang")).ok().as_deref(), Some(LANG));
        let dacl = |p: &Path| {
            let h = oled_sys::datadir::open_raw(p, READ_CONTROL.0).expect("open");
            oled_sys::datadir::read_security(oled_sys::datadir::handle_of(&h)).expect("security").dacl
        };
        assert_eq!(dacl(&inst.join("lang")), dacl(&inst), "lang\\ inherits the install folder's ACL");
        // A lang that is not a real folder is refused before anything is recorded.
        let s = Suite::new("lang-file");
        s.put("lang", b"a file, not a folder");
        let (mut j, mut sink) = s.open();
        let got = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut TestEnv::default());
        assert!(matches!(got, Err(SwapError::Failed { run: None, .. })), "{got:?}");
        assert!(j.records().is_empty());
    }

    #[test]
    fn a_running_component_is_swapped() {
        let s = Suite::new("running");
        let me = std::env::current_exe().expect("the test exe");
        let tasks = s.inst.join("tasks.exe");
        std::fs::copy(&me, &tasks).expect("copy the test exe");
        let mut image = Running::start(&tasks);
        let comp = component(ComponentId::Tasks, NEW);
        let staged = s.staged("tasks.exe", NEW);
        let (mut j, mut sink) = s.open();
        let mut env = TestEnv::default();
        assert_eq!(SWAP, Swap::Replace, "U1s's rule");
        let done = swap_in(&s.inst, &comp, &staged, &mut j, &mut sink, &mut env).expect("swap over a running image");
        assert_eq!(s.read("tasks.exe").as_deref(), Some(NEW));
        assert!(same_bytes(&s.inst.join("tasks.prev.exe"), &me), "the running image's copy is the previous one");
        assert!(image.alive(), "the old image keeps running");
        // The replaced image is in use: it waits under an .old name for the next restart.
        assert_eq!(env.at_restart.len(), 1, "{:?}", env.at_restart);
        let old = &env.at_restart[0];
        assert!(is_swap_path(&s.inst, old) && same_bytes(old, &me), "{}", old.display());
        commit_swap(&mut j, &mut sink, done, &mut env).expect("commit");
    }

    // ------------------------------------------------------------------------------------------------ U1d: selftests

    use std::cell::Cell;
    use std::rc::Rc;

    const FAKE_ENV: &str = "OLED_FAKE_COMPONENT";

    /// A fake component: `OLED_FAKE_COMPONENT=instance:<mutex>` runs like an app's single instance (it takes the
    /// named mutex and keeps running); `selftest:<nonce>` is F-P7's selftest (before any mutex, no window: it
    /// signals the tasks selftest event and exits). Without the variable it returns.
    #[test]
    #[ignore = "a fake component for the selftest tests (OLED_FAKE_COMPONENT=instance:<mutex> | selftest:<nonce>)"]
    fn fake_component() {
        use windows::Win32::System::Threading::CreateMutexW;
        match std::env::var(FAKE_ENV).ok().as_deref().and_then(|v| v.split_once(':')) {
            Some(("instance", mutex)) => {
                // SAFETY: a NUL-terminated name; the handle stays open until the process ends.
                let _held = unsafe { CreateMutexW(None, true, &HSTRING::from(mutex)) }.expect("the instance's mutex");
                std::thread::sleep(Duration::from_secs(30));
            }
            Some(("selftest", nonce)) => oled_sys::selftest::signal("tasks", "selftest", nonce).expect("signal"),
            _ => {}
        }
    }

    fn start_fake(exe: &Path, mode: &str) -> std::io::Result<Child> {
        Command::new(exe)
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(["--ignored", "--exact", "update::suite::tests::fake_component", "--test-threads=1", "-q"])
            .env(FAKE_ENV, mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    struct FakeWaiter {
        signaled: bool,
        waited: Rc<Cell<Option<Duration>>>,
    }
    impl SelftestWait for FakeWaiter {
        fn wait(&self, timeout: Duration) -> bool {
            self.waited.set(Some(timeout));
            self.signaled
        }
    }

    /// The shell and the event, faked.
    #[derive(Default)]
    struct FakeHost {
        squatted: bool,
        no_event: bool,
        shell_fails: bool,
        signaled: bool,
        nonce: String,
        started: Vec<(PathBuf, String)>,
        waited: Rc<Cell<Option<Duration>>>,
    }
    impl SelftestHost for FakeHost {
        type Waiter = FakeWaiter;
        fn waiter(&mut self, app: &str) -> Result<(FakeWaiter, String), WaiterError> {
            if self.squatted {
                return Err(WaiterError::Squatted);
            }
            if self.no_event {
                return Err(WaiterError::Other("no Explorer".into()));
            }
            self.nonce = format!("{app}-{:032x}", 7);
            Ok((FakeWaiter { signaled: self.signaled, waited: self.waited.clone() }, self.nonce.clone()))
        }
        fn start(&mut self, exe: &Path, params: &str) -> Result<(), String> {
            self.started.push((exe.to_path_buf(), params.to_string()));
            if self.shell_fails {
                Err("the shell is not running".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn selftest_outcomes() {
        let dir = Path::new(r"C:\P\m");
        let run = |host: &mut FakeHost, id| run_selftest(dir, id, host);
        // Signaled in time: passed; the exe started is the installed one, with the event's nonce.
        let mut host = FakeHost { signaled: true, ..FakeHost::default() };
        assert_eq!(run(&mut host, ComponentId::Tasks), Selftest::Passed);
        let want = (dir.join("tasks.exe"), format!("--selftest --signal {}", host.nonce));
        assert_eq!(host.started, [want]);
        assert_eq!(host.waited.get(), Some(Duration::from_secs(15)));
        let mut host = FakeHost { signaled: true, ..FakeHost::default() };
        assert_eq!(run(&mut host, ComponentId::Files), Selftest::Passed);
        assert_eq!(host.started[0].0, dir.join("files.exe"));
        // Not signaled: failed.
        let mut host = FakeHost::default();
        assert_eq!(run(&mut host, ComponentId::Tasks), Selftest::Failed);
        // The event name taken, or no event at all: unavailable, and nothing started.
        let (squatted, no_event) =
            (FakeHost { squatted: true, ..FakeHost::default() }, FakeHost { no_event: true, ..FakeHost::default() });
        for mut host in [squatted, no_event] {
            assert!(matches!(run(&mut host, ComponentId::Tasks), Selftest::Unavailable(_)));
            assert!(host.started.is_empty() && host.waited.get().is_none());
        }
        // The shell failing: unavailable, never waited for.
        let mut host = FakeHost { shell_fails: true, signaled: true, ..FakeHost::default() };
        assert!(matches!(run(&mut host, ComponentId::Tasks), Selftest::Unavailable(_)));
        assert!(host.waited.get().is_none());
        // The dock and the language file have no selftest to run.
        for id in [ComponentId::Dock, ComponentId::LangIt] {
            let mut host = FakeHost { signaled: true, ..FakeHost::default() };
            assert!(matches!(run(&mut host, id), Selftest::Unavailable(_)), "{id:?}");
            assert!(host.started.is_empty());
        }
    }

    /// The real event (current user's SID, as Explorer's is in the dock) and a spawner in place of the shell.
    #[derive(Default)]
    struct SpawnHost {
        children: Vec<Child>,
    }
    impl SelftestHost for SpawnHost {
        type Waiter = oled_sys::selftest::Waiter;
        fn waiter(&mut self, app: &str) -> Result<(Self::Waiter, String), WaiterError> {
            let sid = oled_sys::pipe::current_user_sid().map_err(|e| WaiterError::Other(format!("{e:?}")))?;
            oled_sys::selftest::Waiter::create(app, "selftest", &sid).map_err(|e| WaiterError::Other(format!("{e:?}")))
        }
        fn start(&mut self, exe: &Path, params: &str) -> Result<(), String> {
            let nonce = params.strip_prefix("--selftest --signal ").ok_or("not a selftest command line")?;
            self.children.push(start_fake(exe, &format!("selftest:{nonce}")).map_err(|e| e.to_string())?);
            Ok(())
        }
    }
    impl Drop for SpawnHost {
        fn drop(&mut self) {
            for c in &mut self.children {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    #[test]
    fn selftest_runs_beside_a_running_instance() {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
        let dir = TempDir::new("selftest");
        let tasks = dir.path("tasks.exe");
        std::fs::copy(std::env::current_exe().expect("the test exe"), &tasks).expect("copy the test exe");
        // The running instance holds a mutex as an app's single instance does (a name of this test's own).
        let mutex = format!(r"Local\mini-oled-dock-test.instance.{}", oled_sys::pipe::nonce());
        let mut instance = Running(start_fake(&tasks, &format!("instance:{mutex}")).expect("start the instance"));
        let held = || {
            // SAFETY: a NUL-terminated name; the handle is closed at once.
            unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(mutex.as_str())) }
                .map(|h| unsafe { CloseHandle(h) }.is_ok())
                .unwrap_or(false)
        };
        let t = std::time::Instant::now();
        while !held() && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(held(), "the instance runs and holds its mutex");
        // The selftest of the same exe, through the dock's path: it signals and exits within 10 s.
        let mut host = SpawnHost::default();
        let t = std::time::Instant::now();
        assert_eq!(run_selftest(&dir.0, ComponentId::Tasks, &mut host), Selftest::Passed);
        let child = host.children.first_mut().expect("the selftest started");
        let exited = loop {
            if let Some(status) = child.try_wait().expect("wait") {
                break Some(status);
            }
            if t.elapsed() > Duration::from_secs(10) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(exited.is_some_and(|s| s.success()), "the selftest exits within 10 s: {exited:?}");
        assert!(instance.alive() && held(), "the instance still runs");
    }

    #[test]
    fn lang_components_are_checked_by_parsing() {
        let s = Suite::new("lang-check");
        s.put(r"lang\it.lang", LANG);
        // The checker is U1e's (LangFile::parse, the tag and the manifest version); here one that accepts and one
        // that refuses, and one that captures what U1e's does.
        let accept = |_: &[u8]| -> Result<(), String> { Ok(()) };
        let refuse = |_: &[u8]| -> Result<(), String> { Err("not an it.lang of this release".into()) };
        let version = "1.8.0";
        let seen = Cell::new(0usize);
        let tagged = |b: &[u8]| -> Result<(), String> {
            seen.set(b.len());
            if b.starts_with(b"OLNG it") && b.windows(version.len()).any(|w| w == version.as_bytes()) {
                Ok(())
            } else {
                Err("another file".into())
            }
        };
        assert_eq!(check_lang(&s.inst, ComponentId::LangIt, &accept), Selftest::Passed);
        assert_eq!(check_lang(&s.inst, ComponentId::LangIt, &refuse), Selftest::Failed);
        assert_eq!(check_lang(&s.inst, ComponentId::LangIt, &tagged), Selftest::Passed);
        assert_eq!(seen.get(), LANG.len(), "the checker reads the file in place");
        // A file over LANG_MAX_BYTES is not read whole, and a missing one fails.
        s.put(r"lang\it.lang", &vec![b'x'; oled_sys::lang::LANG_MAX_BYTES + 1]);
        assert_eq!(check_lang(&s.inst, ComponentId::LangIt, &accept), Selftest::Failed);
        std::fs::remove_file(s.inst.join(r"lang\it.lang")).expect("remove");
        assert_eq!(check_lang(&s.inst, ComponentId::LangIt, &accept), Selftest::Failed);
    }

    // ------------------------------------------------------------------------------------------------ U1e: --suite-sync

    use oled_trust::manifest::release::{asset_url, Asset, Release};
    use oled_trust::manifest::suite::{encode_manifest, SuiteManifest, SUITE_ASSET};
    use std::collections::HashMap;

    const TASKS: &[u8] = b"tasks.exe of the made-up release";
    const TASKS_2: &[u8] = b"tasks.exe of the same release, rebuilt";
    const FILES: &[u8] = b"files.exe of the made-up release";
    const OLD_FILES: &[u8] = b"files.exe of the release before";

    fn sha(b: &[u8]) -> Digest {
        oled_sys::sha256::sha256(b).expect("sha256")
    }

    /// A real it.lang of version `v` (U1e's check parses it).
    fn lang_of(v: Version) -> Vec<u8> {
        let (a, b, c) = (v.major as u16, v.minor as u16, v.patch as u16);
        oled_sys::lang::encode("it", (a, b, c), &[(1, "uno"), (2, "due")]).expect("it.lang")
    }

    /// The dock and the three components of the made-up release (`lang`: its it.lang).
    fn release_parts(lang: &[u8]) -> [(ComponentId, &[u8]); 4] {
        [
            (ComponentId::Dock, DOCK),
            (ComponentId::Tasks, TASKS),
            (ComponentId::Files, FILES),
            (ComponentId::LangIt, lang),
        ]
    }

    fn manifest_of(v: Version, parts: &[(ComponentId, &[u8])]) -> SuiteManifest {
        let components = parts
            .iter()
            .map(|(id, b)| {
                let stage =
                    if matches!(id, ComponentId::Dock | ComponentId::LangIt) { Stage::Stable } else { Stage::Beta };
                Component { id: *id, version: v, sha256: sha(b), size: b.len() as u64, stage }
            })
            .collect();
        SuiteManifest { schema: 1, version: v, components }
    }

    /// The stage as a temp folder; its reads, writes and removal counted.
    struct DirStage {
        dir: PathBuf,
        reads: usize,
        writes: Vec<String>,
        removed: bool,
    }
    impl DirStage {
        fn put(&self, name: &str, bytes: &[u8]) {
            std::fs::create_dir_all(&self.dir).expect("stage");
            std::fs::write(self.dir.join(name), bytes).expect("stage a file");
        }
        fn get(&self, name: &str) -> Option<Vec<u8>> {
            std::fs::read(self.dir.join(name)).ok()
        }
    }
    impl SuiteStage for DirStage {
        fn read(&mut self, name: &str, cap: u64) -> std::io::Result<Vec<u8>> {
            self.reads += 1;
            let b = std::fs::read(self.dir.join(name))?;
            if b.len() as u64 > cap {
                return Err(std::io::Error::other("over the cap"));
            }
            Ok(b)
        }
        fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
            self.writes.push(name.to_string());
            std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
            std::fs::write(self.dir.join(name), bytes).map_err(|e| e.to_string())
        }
        fn path(&mut self, name: &str) -> std::io::Result<PathBuf> {
            let p = self.dir.join(name);
            std::fs::metadata(&p)?;
            Ok(p)
        }
        fn remove(&mut self) -> Result<(), String> {
            self.removed = true;
            match std::fs::remove_dir_all(&self.dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            }
        }
    }

    /// GitHub, faked: the release (None: no connection) and the assets by URL; every request recorded.
    #[derive(Default)]
    struct FakeNet {
        release: Option<Release>,
        files: HashMap<String, Vec<u8>>,
        calls: Vec<String>,
    }
    impl SuiteNet for FakeNet {
        fn release(&mut self, tag: &str) -> Result<Release, String> {
            self.calls.push(format!("release {tag}"));
            self.release.clone().ok_or_else(|| "no connection".to_string())
        }
        fn get(&mut self, asset: &Asset, _cap: u64) -> Result<Vec<u8>, String> {
            self.calls.push(asset.url.clone());
            self.files.get(&asset.url).cloned().ok_or_else(|| "HTTP 404".to_string())
        }
    }

    fn asset_of(tag: &str, name: &str, bytes: &[u8]) -> Asset {
        Asset { url: asset_url(tag, name), size: bytes.len() as u64, digest: Some(sha(bytes)) }
    }

    impl FakeNet {
        /// The release of `m`'s version with its suite.json (when `with_suite`) and the components' assets.
        fn publish(m: &SuiteManifest, parts: &[(ComponentId, &[u8])], with_suite: bool) -> FakeNet {
            let tag = m.version.tag();
            let text = encode_manifest(m);
            let mut files = HashMap::new();
            let suite = with_suite.then(|| {
                files.insert(asset_url(&tag, SUITE_ASSET), text.as_bytes().to_vec());
                asset_of(&tag, SUITE_ASSET, text.as_bytes())
            });
            let mut component_assets = Vec::new();
            for (id, b) in parts.iter().filter(|(id, _)| *id != ComponentId::Dock) {
                files.insert(asset_url(&tag, id.asset()), b.to_vec());
                component_assets.push((*id, asset_of(&tag, id.asset(), b)));
            }
            let release = Release {
                tag: tag.clone(),
                version: m.version,
                name: String::new(),
                notes: String::new(),
                published: String::new(),
                exe: asset_of(&tag, "dock.exe", DOCK),
                sha256: asset_of(&tag, "dock.exe.sha256", b"sha"),
                suite,
                component_assets,
                dropped: Vec::new(),
            };
            FakeNet { release: Some(release), files, calls: Vec::new() }
        }
    }

    /// The selftests (Passed unless told otherwise), the ARP writer and the apps' windows, faked.
    #[derive(Default)]
    struct FakeOs {
        proofs: HashMap<ComponentId, Selftest>,
        ran: Vec<ComponentId>,
        arp: usize,
        notified: Vec<ComponentId>,
    }
    impl SyncOs for FakeOs {
        fn selftest(&mut self, _dir: &Path, id: ComponentId) -> Selftest {
            self.ran.push(id);
            self.proofs.get(&id).cloned().unwrap_or(Selftest::Passed)
        }
        fn write_arp(&mut self, _dir: &Path) -> Result<(), String> {
            self.arp += 1;
            Ok(())
        }
        fn notify_updated(&mut self, _dir: &Path, id: ComponentId, _v: Version) {
            self.notified.push(id);
        }
    }

    /// One sync's world: a temp install folder (dock.exe = DOCK), its stage, update.jnl, GitHub and the machine faked.
    struct World {
        s: Suite,
        stage: DirStage,
        net: FakeNet,
        os: FakeOs,
        network: bool,
    }
    impl World {
        fn new(tag: &str) -> World {
            let s = Suite::new(tag);
            let stage = DirStage { dir: s.stage.clone(), reads: 0, writes: Vec::new(), removed: false };
            World { s, stage, net: FakeNet::default(), os: FakeOs::default(), network: false }
        }
        fn run(&mut self) -> SyncResult {
            let cx =
                SyncCtx { dir: &self.s.inst, journal: &self.s.jnl, version: Version::current(), network: self.network };
            sync_in(&cx, &mut self.net, &mut self.stage, &mut self.os, &mut TestEnv::default())
        }
        /// `m`'s suite.json and the assets of `parts` (the dock aside) in the stage.
        fn stage_release(&self, m: &SuiteManifest, parts: &[(ComponentId, &[u8])]) {
            self.stage.put(SUITE_ASSET, encode_manifest(m).as_bytes());
            for (id, b) in parts.iter().filter(|(id, _)| *id != ComponentId::Dock) {
                self.stage.put(id.asset(), b);
            }
        }
    }

    fn line(r: &SyncResult) -> &str {
        r.line.as_str()
    }

    #[test]
    fn sync_outcomes() {
        let v = Version::current();
        let lang = lang_of(v);
        let parts = release_parts(&lang);
        let m = manifest_of(v, &parts);

        // A first run, nothing installed: the stage's three components added and proved, suite.json written,
        // update.jnl gone, the ARP entry written once, the stage removed.
        let mut w = World::new("sync-first");
        w.stage_release(&m, &parts);
        let r = w.run();
        assert_eq!((r.code, line(&r)), (EXIT_OK, "suite-sync: ok added=tasks,files,lang-it"));
        assert_eq!(w.s.read("tasks.exe").as_deref(), Some(TASKS));
        assert_eq!(w.s.read("files.exe").as_deref(), Some(FILES));
        assert_eq!(w.s.read(r"lang\it.lang"), Some(lang.clone()));
        assert_eq!(w.s.read("suite.json"), Some(encode_manifest(&m).into_bytes()));
        assert!(!w.s.jnl.exists(), "update.jnl deleted");
        assert_eq!(w.os.arp, 1, "the ARP entry written once");
        assert_eq!(
            w.os.ran,
            [ComponentId::Tasks, ComponentId::Files],
            "the exes' selftests; the language file is parsed"
        );
        assert_eq!(w.os.notified, [ComponentId::Tasks, ComponentId::Files, ComponentId::LangIt]);
        assert!(w.stage.removed && !w.stage.dir.exists(), "the stage goes after an ok");
        assert!(w.net.calls.is_empty(), "nothing fetched");

        // A failing tasks selftest: tasks put back (its file gone), files and lang-it kept, a strike, exit 1.
        let mut w = World::new("sync-fail");
        w.stage_release(&m, &parts);
        w.os.proofs.insert(ComponentId::Tasks, Selftest::Failed);
        let r = w.run();
        assert_eq!(r.code, EXIT_FAILED);
        assert_eq!(
            line(&r),
            "suite-sync: failed added=files,lang-it reverted=tasks (tasks: its selftest failed, strike 1)"
        );
        assert!(w.s.read("tasks.exe").is_none(), "tasks reverted");
        assert_eq!(w.s.read("files.exe").as_deref(), Some(FILES));
        assert_eq!(w.stage.get("bad-tasks"), Some(format!("{} 1\n", hex(&sha(TASKS))).into_bytes()));
        assert_eq!(w.os.arp, 1, "files and lang-it changed the folder");
        assert!(!w.stage.removed, "the stage stays for the next try");
        assert!(!w.s.jnl.exists(), "every run committed or put back: update.jnl deleted");
        // The second and the third failure of the same build; then the build is skipped (no selftest runs).
        for n in [2, 3] {
            w.os.ran.clear();
            let r = w.run();
            assert!(line(&r).ends_with(&format!("(tasks: its selftest failed, strike {n})")), "{}", r.line);
        }
        w.os.ran.clear();
        let r = w.run();
        assert_eq!(
            (r.code, line(&r)),
            (EXIT_OK, "suite-sync: pending pending=tasks (tasks: skipped after three failed proofs)")
        );
        assert!(w.os.ran.is_empty(), "no selftest for a build with three strikes");
        // Until a new build of tasks: another hash in this release's manifest is tried again (and here passes).
        let m2 = manifest_of(v, &[parts[0], (ComponentId::Tasks, TASKS_2), parts[2], parts[3]]);
        std::fs::write(w.s.inst.join(SUITE_ASSET), encode_manifest(&m2)).expect("suite.json");
        w.stage.put("tasks.exe", TASKS_2);
        w.os.proofs.clear();
        let r = w.run();
        assert_eq!((r.code, line(&r)), (EXIT_OK, "suite-sync: ok added=tasks"));
        assert_eq!(w.s.read("tasks.exe").as_deref(), Some(TASKS_2));

        // No selftest can be asked for (the shell is not running): everything this run added is put back, no strike,
        // exit 3, and the run stops.
        let mut w = World::new("sync-noshell");
        w.stage_release(&m, &parts);
        w.os.proofs.insert(ComponentId::Tasks, Selftest::Unavailable("the shell is not running".into()));
        let before = snapshot(&w.s.inst);
        let r = w.run();
        assert_eq!(r.code, EXIT_RETRY);
        assert_eq!(line(&r), "suite-sync: unavailable reverted=tasks (no selftest: the shell is not running)");
        for f in ["tasks.exe", "files.exe", r"lang\it.lang"] {
            assert!(w.s.read(f).is_none(), "{f} not installed");
        }
        assert!(w.stage.get("bad-tasks").is_none(), "no strike");
        assert_eq!(w.os.arp, 0);
        let after: BTreeMap<_, _> = snapshot(&w.s.inst).into_iter().filter(|(n, _)| n != SUITE_ASSET).collect();
        assert_eq!(after, before, "the install folder as it was (but the manifest it follows)");

        // The release's dock is not the installed dock.exe (a dev build): nothing touched, exit 4.
        let mut w = World::new("sync-devbuild");
        w.network = true;
        let other = manifest_of(v, &[(ComponentId::Dock, b"the published dock.exe"), parts[1], parts[2], parts[3]]);
        w.net = FakeNet::publish(&other, &parts, true);
        w.s.put("files.exe", OLD_FILES);
        let before = snapshot(&w.s.inst);
        let r = w.run();
        assert_eq!(
            (r.code, line(&r)),
            (EXIT_REFUSED, "suite-sync: refused (this dock.exe is not the published build)")
        );
        assert_eq!(snapshot(&w.s.inst), before, "nothing touched");
        assert_eq!(w.net.calls, [format!("release {}", v.tag()), asset_url(&v.tag(), SUITE_ASSET)]);
        assert!(!w.s.jnl.exists() && w.os.arp == 0);

        // A release without suite.json (every release before the suite): exit 4, nothing removed.
        let mut w = World::new("sync-nosuite");
        w.network = true;
        w.net = FakeNet::publish(&m, &parts, false);
        w.s.put("files.exe", OLD_FILES);
        let before = snapshot(&w.s.inst);
        let r = w.run();
        assert_eq!((r.code, line(&r)), (EXIT_REFUSED, "suite-sync: refused (the release has no suite.json)"));
        assert_eq!(snapshot(&w.s.inst), before, "nothing removed");

        // No network and nothing staged but the manifest: each component pending, exit 0, nothing installed.
        let mut w = World::new("sync-pending");
        w.stage.put(SUITE_ASSET, encode_manifest(&m).as_bytes());
        let r = w.run();
        assert_eq!(r.code, EXIT_OK);
        assert_eq!(
            line(&r),
            "suite-sync: pending pending=tasks,files,lang-it (tasks: not staged, no network; files: not staged, no \
             network; lang-it: not staged, no network)"
        );
        assert!(w.s.read("tasks.exe").is_none() && w.os.arp == 0 && w.net.calls.is_empty());
        // Not even the manifest: pending, nothing read from the network, nothing written.
        let mut w = World::new("sync-nothing");
        let before = snapshot(&w.s.inst);
        let r = w.run();
        assert_eq!(
            (r.code, line(&r)),
            (EXIT_OK, "suite-sync: pending (no manifest of this release: not staged, no network)")
        );
        assert_eq!(snapshot(&w.s.inst), before);

        // With the network: the release fetched by tag, its manifest and the components downloaded, checked and
        // staged, then installed.
        let mut w = World::new("sync-download");
        w.network = true;
        w.net = FakeNet::publish(&m, &parts, true);
        let r = w.run();
        assert_eq!(
            (r.code, line(&r)),
            (EXIT_OK, "suite-sync: ok added=tasks,files,lang-it (downloaded tasks,files,lang-it; release fetched)")
        );
        assert_eq!(w.s.read("files.exe").as_deref(), Some(FILES));
        // A component file that cannot be read (here a folder in its place) is pending, once, and left alone.
        let mut w = World::new("sync-unreadable");
        w.stage_release(&m, &parts);
        std::fs::create_dir_all(w.s.inst.join("tasks.exe")).expect("a folder named tasks.exe");
        let r = w.run();
        assert!(line(&r).starts_with("suite-sync: pending added=files,lang-it pending=tasks (tasks: "), "{}", r.line);
        assert_eq!(line(&r).matches("tasks: ").count(), 1, "{}", r.line);
        assert!(w.s.inst.join("tasks.exe").is_dir() && !w.os.ran.contains(&ComponentId::Tasks));

        // The release cannot be fetched: asked for once, every component pending.
        let mut w = World::new("sync-offline");
        w.network = true;
        w.stage.put(SUITE_ASSET, encode_manifest(&m).as_bytes());
        let r = w.run();
        let want =
            "suite-sync: pending pending=tasks,files,lang-it (release fetched; tasks: the release: no connection;";
        assert!(line(&r).starts_with(want), "{}", r.line);
        assert_eq!(w.net.calls, [format!("release {}", v.tag())], "one fetch for the run");

        // A download that is not the manifest's file is never installed.
        let mut w = World::new("sync-forged");
        w.network = true;
        w.net = FakeNet::publish(&m, &parts, true);
        let mut forged = FILES.to_vec();
        forged[0] ^= 1; // the same length, other bytes
        w.net.files.insert(asset_url(&v.tag(), "files.exe"), forged);
        let r = w.run();
        assert_eq!(r.code, EXIT_OK);
        assert!(line(&r).starts_with("suite-sync: pending added=tasks,lang-it pending=files ("), "{}", r.line);
        assert!(w.s.read("files.exe").is_none() && w.stage.get("files.exe").is_none());
    }

    #[test]
    fn a_rollback_converges_from_the_previous_copies() {
        // The newer components are in place, the older ones are their previous copies (a --rollback's dock): the sync
        // swaps them back without a download.
        let v = Version::current();
        let lang = lang_of(v);
        let parts = release_parts(&lang);
        let m = manifest_of(v, &parts);
        let mut w = World::new("sync-rollback");
        w.network = true;
        w.net = FakeNet::publish(&m, &parts, true);
        for (id, newer) in [(ComponentId::Tasks, &b"tasks.exe, newer"[..]), (ComponentId::Files, b"files.exe, newer")] {
            w.s.put(id.file(), newer);
            w.s.put(id.prev(), parts.iter().find(|(p, _)| *p == id).map(|(_, b)| *b).expect("part"));
        }
        w.s.put(r"lang\it.lang", b"OLNG of the newer release");
        w.s.put(r"lang\it.prev.lang", &lang);
        let r = w.run();
        assert_eq!(
            (r.code, line(&r)),
            (EXIT_OK, "suite-sync: ok replaced=tasks,files,lang-it (from prev; release fetched)")
        );
        assert_eq!(
            w.net.calls,
            [format!("release {}", v.tag()), asset_url(&v.tag(), SUITE_ASSET)],
            "no component download"
        );
        assert_eq!(w.s.read("tasks.exe").as_deref(), Some(TASKS));
        assert_eq!(w.s.read("tasks.prev.exe").as_deref(), Some(&b"tasks.exe, newer"[..]));
        assert_eq!(w.s.read(r"lang\it.lang"), Some(lang));
    }

    #[test]
    fn converged_run_reads_no_network() {
        let v = Version::current();
        let lang = lang_of(v);
        let parts = release_parts(&lang);
        let m = manifest_of(v, &parts);
        let mut w = World::new("sync-converged");
        w.network = true;
        w.net = FakeNet::publish(&m, &parts, true);
        for (id, b) in &parts[1..] {
            w.s.put(id.file(), b);
        }
        w.s.put(SUITE_ASSET, encode_manifest(&m).as_bytes());
        w.stage_release(&m, &parts);
        let (inst, stage) = (snapshot(&w.s.inst), snapshot(&w.stage.dir));
        HASHED.with(|h| h.set(0));
        let r = w.run();
        assert_eq!((r.code, line(&r)), (EXIT_OK, "suite-sync: ok"));
        assert!(w.net.calls.is_empty(), "no network");
        assert_eq!((w.stage.reads, w.stage.writes.len()), (0, 0), "the stage is never read");
        assert_eq!(HASHED.with(|h| h.get()), 4, "one hash per installed component (the dock and three)");
        assert_eq!((snapshot(&w.s.inst), snapshot(&w.stage.dir)), (inst, stage), "no write at all");
        assert!(!w.s.jnl.exists(), "update.jnl never created");
        assert_eq!((w.os.arp, w.os.ran.len(), w.os.notified.len()), (0, 0, 0));
    }

    #[test]
    fn install_suite_json_must_match() {
        let v = Version::current();
        let lang = lang_of(v);
        let parts = release_parts(&lang);
        let m = manifest_of(v, &parts);
        let older = Version { minor: v.minor.wrapping_sub(1), ..v };
        let other_version = manifest_of(older, &[(ComponentId::Dock, DOCK), (ComponentId::Tasks, OLD_FILES)]);
        let other_dock = manifest_of(v, &[(ComponentId::Dock, b"another dock.exe"), (ComponentId::Tasks, OLD_FILES)]);
        for (case, installed) in [("version", other_version), ("dock", other_dock)] {
            let mut w = World::new(&format!("sync-installed-{case}"));
            w.s.put(SUITE_ASSET, encode_manifest(&installed).as_bytes());
            w.stage_release(&m, &parts);
            let r = w.run();
            assert_eq!((r.code, line(&r)), (EXIT_OK, "suite-sync: ok added=tasks,files,lang-it"), "{case}");
            assert_eq!(
                w.s.read(SUITE_ASSET),
                Some(encode_manifest(&m).into_bytes()),
                "{case}: the own manifest written"
            );
            assert!(w.stage.reads > 0, "{case}: the stage was used");
        }
        // And the network, when nothing is staged.
        let mut w = World::new("sync-installed-net");
        w.network = true;
        w.net = FakeNet::publish(&m, &parts, true);
        w.s.put(SUITE_ASSET, encode_manifest(&manifest_of(v, &[(ComponentId::Dock, b"a dev dock")])).as_bytes());
        let r = w.run();
        assert!(line(&r).starts_with("suite-sync: ok added=tasks,files,lang-it ("), "{}", r.line);
    }

    #[test]
    fn a_second_sync_is_busy() {
        let dir = TempDir::new("sync-lock");
        let lock = dir.path("suite.lock");
        let held = take_lock(&lock).expect("the first sync's lock");
        let mut ran = false;
        let r = locked(&lock, || {
            ran = true;
            SyncResult { code: EXIT_OK, line: String::new() }
        });
        assert_eq!((r.code, line(&r)), (EXIT_RETRY, "suite-sync: busy (another sync runs)"));
        assert!(!ran, "nothing done");
        drop(held);
        let r = locked(&lock, || SyncResult { code: EXIT_OK, line: "suite-sync: ok".into() });
        assert_eq!(r.code, EXIT_OK, "free again once the first sync ended");
        // update.jnl held by another writer past LOCK_WAIT is busy too (open_journal), never an error.
        let jnl = dir.path("update.jnl");
        let _writer = open_locked_at(&jnl, Scope::Machine, LOCK).expect("update.jnl");
        let r = open_journal(&jnl).map(|_| ()).expect_err("held");
        assert_eq!((r.code, line(&r)), (EXIT_RETRY, "suite-sync: busy (update.jnl is held by another process)"));
    }

    #[test]
    fn cleanup_keeps_the_running_stage_until_converged() {
        let v = Version::current();
        let own = v.tag();
        let stale = || vec!["v0.0.1".to_string(), own.clone()];
        // The start's cleanup asks only about the running version's stage, and keeps it while it is needed.
        let asked = std::cell::RefCell::new(Vec::new());
        let keep = crate::update::removable_stages(stale(), v, |t| {
            asked.borrow_mut().push(t.to_string());
            true
        });
        assert_eq!(keep, ["v0.0.1"]);
        assert_eq!(asked.borrow().as_slice(), std::slice::from_ref(&own));
        assert_eq!(crate::update::removable_stages(stale(), v, |_| false), stale());
        // The stage holds a component that is not installed: needed; the sync installs it, says ok, removes the stage.
        let lang = lang_of(v);
        let parts = release_parts(&lang);
        let m = manifest_of(v, &parts);
        let mut w = World::new("sync-cleanup");
        w.stage_release(&m, &parts);
        w.s.put("files.exe", FILES);
        assert!(stage_holds_new_components(&mut w.stage, &w.s.inst), "tasks and lang-it are not installed");
        let r = w.run();
        assert_eq!(line(&r), "suite-sync: ok added=tasks,lang-it");
        assert!(w.stage.removed && !w.stage.dir.exists(), "removed after the ok");
        // A stage whose components are all in place is not needed, unless it holds the strikes of a build that is not.
        w.stage_release(&m, &parts);
        assert!(!stage_holds_new_components(&mut w.stage, &w.s.inst));
        w.stage.put("bad-files", format!("{} 3\n", hex(&sha(TASKS_2))).as_bytes());
        assert!(stage_holds_new_components(&mut w.stage, &w.s.inst), "the strikes of another build of files");
        w.stage.put("bad-files", format!("{} 1\n", hex(&sha(FILES))).as_bytes());
        assert!(!stage_holds_new_components(&mut w.stage, &w.s.inst), "the build in place passed later");
    }

    #[test]
    fn stage_includes_changed_components() {
        let v = Version { major: 99, minor: 1, patch: 0 };
        let lang = lang_of(v);
        let parts: [(ComponentId, &[u8]); 4] = [
            (ComponentId::Dock, b"dock.exe 99.1.0"),
            (ComponentId::Tasks, TASKS),
            (ComponentId::Files, FILES),
            (ComponentId::LangIt, &lang),
        ];
        let m = manifest_of(v, &parts);
        let s = Suite::new("stage-suite");
        s.put("tasks.exe", TASKS); // unchanged: never downloaded
        s.put("files.exe", OLD_FILES); // changed
        let mut stage = DirStage { dir: s.stage.join("v99.1.0"), reads: 0, writes: Vec::new(), removed: false };
        let mut net = FakeNet::publish(&m, &parts, true);
        let Some(rel) = net.release.clone() else { panic!("the release") };
        assert_eq!(stage_components(&rel, &s.inst, &mut net, &mut stage), Ok(3));
        let tag = v.tag();
        assert_eq!(
            net.calls,
            [asset_url(&tag, SUITE_ASSET), asset_url(&tag, "files.exe"), asset_url(&tag, "lang-it.lang")],
            "tasks.exe is in place: not downloaded"
        );
        assert_eq!(stage.get(SUITE_ASSET), Some(encode_manifest(&m).into_bytes()));
        assert_eq!(stage.get("files.exe").as_deref(), Some(FILES));
        assert_eq!(stage.get("lang-it.lang"), Some(lang.clone()));
        assert!(stage.get("tasks.exe").is_none());
        // Staged already: nothing downloaded again.
        net.calls.clear();
        assert_eq!(stage_components(&rel, &s.inst, &mut net, &mut stage), Ok(0));
        assert!(net.calls.is_empty());
        // Each download is checked: one that is not the manifest's file (or not GitHub's digest) is not staged.
        let mut stage = DirStage { dir: s.stage.join("v99.1.0-b"), reads: 0, writes: Vec::new(), removed: false };
        net.files.insert(asset_url(&tag, "lang-it.lang"), vec![b'x'; lang.len()]);
        let r = stage_components(&rel, &s.inst, &mut net, &mut stage);
        assert!(r.as_ref().is_err_and(|e| e.starts_with("lang-it: ")), "{r:?}");
        assert!(stage.get("lang-it.lang").is_none() && stage.get("files.exe").is_some());
        // A release without a manifest stages nothing.
        let bare = Release { suite: None, ..rel };
        let mut stage = DirStage { dir: s.stage.join("v99.1.0-c"), reads: 0, writes: Vec::new(), removed: false };
        assert_eq!(stage_components(&bare, &s.inst, &mut net, &mut stage), Ok(0));
        assert!(stage.writes.is_empty());
    }

    #[test]
    fn the_dock_reads_the_result_line() {
        // One line of text with the prefix, at most 512 bytes; anything else is said with the exit code.
        assert_eq!(result_line(0, b"suite-sync: ok in 12 ms\r\n"), "suite-sync: ok in 12 ms");
        assert_eq!(result_line(1, b""), "suite-sync: error (exit 1 without a result line)");
        assert_eq!(result_line(0, b"ok\n"), "suite-sync: error (exit 0 without a result line)");
        assert_eq!(result_line(0, b"suite-sync: ok\x1b[2J\n"), "suite-sync: error (exit 0 without a result line)");
        let long = format!("suite-sync: ok ({})", "x".repeat(RESULT_MAX));
        assert_eq!(result_line(0, long.as_bytes()), "suite-sync: error (exit 0 without a result line)");
        // The printed line: the time at the end, within RESULT_MAX bytes, control characters as spaces.
        assert_eq!(finish_line("suite-sync: ok", Duration::from_millis(180)), "suite-sync: ok in 180 ms");
        let printed = finish_line(&format!("suite-sync: failed (\u{e8}{}\n)", "\u{e8}".repeat(600)), Duration::ZERO);
        assert!(printed.len() <= RESULT_MAX && printed.ends_with(" in 0 ms") && !printed.contains('\n'));
        assert_eq!(result_line(1, printed.as_bytes()), printed);
        // What the dock keeps of it.
        let f = result_facts("suite-sync: failed added=files,lang-it reverted=tasks (tasks: x=y) in 9 ms");
        assert_eq!((f.state.as_str(), f.changed), ("failed", true));
        assert_eq!(f.not_current, [ComponentId::Tasks]);
        let f = result_facts("suite-sync: ok in 3 ms");
        assert_eq!((f.state.as_str(), f.changed, f.not_current.len()), ("ok", false, 0));
        // Components: the manifest's version and stage for a file in place the last sync did not leave behind.
        let v = Version::current();
        let m = manifest_of(v, &[(ComponentId::Dock, DOCK), (ComponentId::Tasks, TASKS), (ComponentId::LangIt, b"it")]);
        let c = components_of(Some(&m), |id| id != ComponentId::Files, &[ComponentId::LangIt]);
        let ids: Vec<_> = c.iter().map(|c| c.id).collect();
        assert_eq!(ids, [ComponentId::Tasks, ComponentId::TasksAdmin, ComponentId::Files, ComponentId::LangIt]);
        assert_eq!((c[0].installed, c[0].version.clone(), c[0].stage), (true, Some(v.to_string()), Some(Stage::Beta)));
        assert_eq!((c[1].installed, c[1].version.clone(), c[1].stage), (true, None, None), "not in the manifest");
        assert_eq!((c[2].installed, c[2].version.clone()), (false, None));
        assert_eq!((c[3].version.clone(), c[3].stage), (None, Some(Stage::Stable)), "left at another build");
    }

    #[test]
    fn suite_manifest_script_matches_the_parser() {
        // tools/ci/suite-manifest.ps1 -SelfTest writes this text for two dummy files ("abc" as dock.exe, "a made-up
        // tasks.exe" as tasks.exe, version 1.8.0, stage beta) and compares it byte for byte (ci.yml); the dock's parser
        // reads it and its encoder writes it back unchanged.
        const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/update/suite-selftest.json");
        let m = parse_manifest(FIXTURE).expect("the script's manifest parses");
        assert_eq!(encode_manifest(&m).as_bytes(), FIXTURE, "the script and the encoder agree byte for byte");
        let v = Version { major: 1, minor: 8, patch: 0 };
        let want = manifest_of(v, &[(ComponentId::Dock, b"abc"), (ComponentId::Tasks, b"a made-up tasks.exe")]);
        assert_eq!(m, want);
    }
}
