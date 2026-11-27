//! Persistent data folder `%ProgramData%\mini-oled-dock` and handle-based file I/O inside it.
//!
//! WHY: the dock runs elevated. Its data used to live in `%LOCALAPPDATA%\Dock`, which every
//! medium-integrity process of the same user can write: planting a junction, symlink or hard link
//! there turns the elevated writes, renames and deletes into an arbitrary file write/delete (a
//! classic local privilege escalation). The new root is created by the elevated process with an
//! explicit PROTECTED DACL (nothing inherited from `ProgramData`, where Users may create folders):
//! SYSTEM and Administrators full control, Users read and execute only. Non-admins cannot create,
//! rename or delete entries in it, so nothing can be planted.
//!
//! LAYOUT:
//!  * `<root>\config.json`: the only file the interactive user may change. Its own protected DACL
//!    adds one ACE for the token user: read + write data (no DELETE, no WRITE_DAC). Every
//!    programmatic write creates a temp file with that same DACL and renames it over the old one
//!    (`write_atomic`), so the ACE is never lost and the replace is atomic.
//!  * `<root>\config.json.bak`, `dock.log`, `dock.log.1`, `taskbar-state`, `cache\`: admin-only
//!    write (they inherit the root DACL).
//!  * `<root>\cache\<SID>\`: the launcher's history of the shell user with that SID
//!    (`user_cache_dir`): its own protected DACL, SYSTEM and Administrators full control, that user
//!    read only, no ACE for BUILTIN\Users (who can read the rest of the root).
//!  * `<root>\icons\`: extra ACEs let the token user add files (and change the files inside). Since
//!    the user can create entries there, custom icons are opened WITHOUT following reparse points,
//!    must be single-link regular files whose final path is exactly `<icons>\<name>`, and are decoded
//!    from memory (`read_icon`). The elevated process never writes into `icons\`.
//!  * `<root>\update\<tag>\`: releases staged by the updater (`dock.exe`, `dock.exe.sha256`), the update
//!    guard's `dock-guard.exe` and its `rolled-back` mark, admin-only (inherited root DACL). A staged file is
//!    read or executed only through the checked path (`read_stage_file`, `checked_stage_path`).
//!
//! MODES: `init()` runs once, in the processes that need persistent data (dock, watchdog,
//! `--install`/`--uninstall`, `--restore`). Elevated: creates, verifies and repairs the folders.
//! Not elevated: verifies only and never writes (an unelevated create would itself squat the root,
//! since ProgramData lets Users create folders). If the root is unusable every path is `None`:
//! config defaults, no cache, log only to the debugger output. Before `init()` every path is
//! `None` too, so `--preview` and `--list-apps` never create or touch the root.
//!
//! SQUATTING: a medium process could pre-create the root before the first install. A root whose
//! owner is not Administrators/SYSTEM is hostile: it is renamed away through the very handle that
//! was inspected (no path race, no follow of reparse points) and recreated; if that fails it is
//! refused with a message telling the user to delete it from an elevated prompt. An admin-owned root
//! whose DACL lets non-admins write is treated the same way (moved aside as `.old-...`, recreated):
//! anything inside may have been planted while it was loose, so nothing in it is reused except its
//! `config.json`, read back through the checked path (`read_previous_config`).
//!
//! THREADING: everything is thread-safe (`OnceLock` state, no COM).

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use oled_sys::datadir::RootInfo;
// The mechanism lives in oled-sys; everything below keeps its old path through these re-exports.
use oled_sys::datadir::{
    checked_dir, classify, dir_facts, final_path, final_path_matches, handle_of, is_admin_sid, is_sid_string, open_dir,
    read_capped, read_security, token_user_sid, OpenDir, SecDesc, Verdict, MASK_READ_EXECUTE,
};
pub use oled_sys::datadir::{
    debug_out, is_admin_only_dir, is_plain_dir_name, known_folder, open_append, open_checked, open_checked_read_only,
    read_file, write_atomic, FileAcl, ROOT_NAME,
};

use windows::core::{BOOL, HSTRING};
use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
use windows::Win32::Security::{
    SetKernelObjectSecurity, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::{
    CopyFileW, CreateDirectoryW, FileRenameInfo, MoveFileExW, SetFileInformationByHandle, DELETE,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_RENAME_INFO, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, READ_CONTROL, SYNCHRONIZE,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_ProgramData};

const CACHE_NAME: &str = "cache";
const ICONS_NAME: &str = "icons";
/// `<root>\update\<tag>\`: staged releases (admin-only, inherited root DACL).
const UPDATE_NAME: &str = "update";
/// `<root>\suite`: the suite's shared files (theme.json, the manifest copy). Admin-only (inherited root DACL).
const SUITE_NAME: &str = "suite";
/// `<root>\journal`: the machine change journal. Admin-only (inherited root DACL).
const JOURNAL_NAME: &str = "journal";
/// The only files a staging folder holds: the release's two assets, the update guard's copy of the old
/// exe and the mark of a rolled-back update (`update_release::GUARD_FILE`, `ROLLED_BACK_FILE`).
const STAGE_FILES: [&str; 13] = [
    "dock.exe",
    "dock.exe.sha256",
    "dock-guard.exe",
    "rolled-back",
    // The suite's staged components, its manifest, and the marks of components that failed their selftest.
    "suite.json",
    "tasks.exe",
    "tasks-admin.exe",
    "files.exe",
    "lang-it.lang",
    "bad-tasks",
    "bad-tasks-admin",
    "bad-files",
    "bad-lang-it",
];

/// Size caps of the files read into memory.
pub const CONFIG_MAX_BYTES: u64 = 1024 * 1024;
pub const ICON_MAX_BYTES: u64 = 4 * 1024 * 1024;
pub const CACHE_MAX_BYTES: u64 = 1024 * 1024;
pub const STATE_MAX_BYTES: u64 = 64;

// ------------------------------------------------------------------ access masks (pure)

/// The user's ACE on the `icons` folder itself: list (1), add file (2), read EA (8), traverse (0x20),
/// read attributes (0x80), READ_CONTROL, SYNCHRONIZE. No add-subfolder, no delete-child, no WRITE_DAC.
const MASK_ICONS_DIR_USER: u32 = 0x1 | 0x2 | 0x8 | 0x20 | 0x80 | READ_CONTROL.0 | SYNCHRONIZE.0;
/// Inherit-only ACE for the files inside `icons`: read, write and delete them (so the user can replace
/// or remove the PNGs they dropped). No WRITE_DAC beyond what the owner of a file implicitly has.
const MASK_ICONS_FILE_USER: u32 = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0 | DELETE.0;

/// SDDL of the root: owner Administrators, protected DACL, inheritable to files and subfolders.
fn root_sddl() -> String {
    format!("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x{MASK_READ_EXECUTE:x};;;BU)")
}

/// SDDL of `cache\<user>` (the launcher's history): SYSTEM and Administrators full control, `user` read
/// only, inherited by the files; no BUILTIN\Users ACE, so no other account reads it.
fn user_cache_sddl(user: &str) -> Option<String> {
    is_sid_string(user).then(|| format!("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;{user})"))
}

/// SDDL of `icons\`: the root ACEs plus "add files" on the folder and an inherit-only ACE that gives
/// `user` read/write/delete on the files created inside.
fn icons_sddl(user: &str) -> Option<String> {
    is_sid_string(user).then(|| {
        format!(
            "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x{MASK_READ_EXECUTE:x};;;BU)\
             (A;;0x{MASK_ICONS_DIR_USER:x};;;{user})(A;OIIO;0x{MASK_ICONS_FILE_USER:x};;;{user})"
        )
    })
}

// ------------------------------------------------------------------ state

struct Root {
    path: PathBuf,
    /// Elevated and verified: the process may write (config, cache, log, state).
    writable: bool,
    /// `cache\` verified (admin-only, real folder).
    cache_ok: bool,
    /// Final path of `icons\` (for `read_icon`); `None` = custom icons disabled.
    icons_final: Option<String>,
    /// Token user as string SID (ACE on config.json).
    user_sid: Option<String>,
    /// Final path of an admin-owned root moved aside this run because its DACL was loose.
    previous_final: Option<String>,
}

static ROOT: OnceLock<Option<Root>> = OnceLock::new();

fn root_ref() -> Option<&'static Root> {
    ROOT.get().and_then(Option::as_ref)
}

/// Sets up the data root (create/verify/repair if elevated, verify only otherwise). Returns the
/// diagnostic lines to log once the logger runs (they are also sent to the debugger output now).
/// Only the first call does anything.
pub fn init() -> Vec<String> {
    if ROOT.get().is_some() {
        return Vec::new();
    }
    let mut notes = Vec::new();
    let r = init_root(&mut notes);
    let info =
        r.as_ref().map(|r| RootInfo { path: r.path.clone(), writable: r.writable, user_sid: r.user_sid.clone() });
    let _ = ROOT.set(r);
    oled_sys::datadir::register(info);
    for n in &notes {
        debug_out(n);
    }
    notes
}

/// true if this process may write in the root (elevated, root verified).
pub fn writable() -> bool {
    root_ref().is_some_and(|r| r.writable)
}

/// `<root>\config.json`.
pub fn config_path() -> Option<PathBuf> {
    root_ref().map(|r| r.path.join("config.json"))
}

/// `<root>\cache` (icon PNG cache), only if verified.
pub fn cache_dir() -> Option<PathBuf> {
    root_ref().filter(|r| r.cache_ok).map(|r| r.path.join(CACHE_NAME))
}

/// `<root>\cache\<sid>`, the launcher's history folder of the shell user `sid`, created or checked and
/// re-secured with `user_cache_sddl` (elevated, verified root only). Anything that is not a SID string is
/// refused before it reaches a path or an SDDL string.
pub fn user_cache_dir(sid: &str) -> Result<PathBuf, String> {
    let sddl = user_cache_sddl(sid).ok_or_else(|| "not a SID".to_string())?;
    let r = writable_root()?;
    if !r.cache_ok {
        return Err("no verified cache folder".into());
    }
    let path = r.path.join(CACHE_NAME).join(sid);
    let mut notes = Vec::new();
    match ensure_subdir(&path, Some(&sddl), true, &mut notes) {
        Some(_) => Ok(path),
        None => Err(notes.join("; ")),
    }
}

/// `<root>\dock.log`, only in a writable root.
pub fn log_path() -> Option<PathBuf> {
    root_ref().filter(|r| r.writable).map(|r| r.path.join("dock.log"))
}

/// `<root>\taskbar-state`.
pub fn state_path() -> Option<PathBuf> {
    root_ref().map(|r| r.path.join("taskbar-state"))
}

/// `<root>\safe-mode`: the mark of safe mode (J1d).
#[allow(dead_code)] // read by J1d and U1
pub fn safe_mode_path() -> Option<PathBuf> {
    root_ref().map(|r| safe_mode_in(&r.path))
}

/// `<root>\update\suite.lock`: the lock of one suite sync at a time (U1).
#[allow(dead_code)] // read by J1d and U1
pub fn update_lock_path() -> Option<PathBuf> {
    root_ref().map(|r| update_lock_in(&r.path))
}

/// `<root>\update.jnl`: the update journal (J1, Scope::Machine format).
#[allow(dead_code)] // read by J1d and U1
pub fn update_journal_path() -> Option<PathBuf> {
    root_ref().map(|r| update_journal_in(&r.path))
}

fn safe_mode_in(root: &Path) -> PathBuf {
    root.join("safe-mode")
}

fn update_lock_in(root: &Path) -> PathBuf {
    root.join(UPDATE_NAME).join("suite.lock")
}

fn update_journal_in(root: &Path) -> PathBuf {
    root.join("update.jnl")
}

/// Old data folder `%LOCALAPPDATA%\Dock` (medium-writable: only read, for the one-time migration).
/// Works without `init()`.
pub fn legacy_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_LocalAppData).map(|p| p.join("Dock"))
}

// ------------------------------------------------------------------ init

fn init_root(notes: &mut Vec<String>) -> Option<Root> {
    let elevated = crate::shell_thread::is_elevated();
    let Some(pd) = known_folder(&FOLDERID_ProgramData) else {
        notes.push("data: %ProgramData% not found: running without persistent data".into());
        return None;
    };
    let path = pd.join(ROOT_NAME);
    let user_sid = token_user_sid();
    if user_sid.is_none() {
        notes.push("data: the user SID could not be read: config.json and icons\\ stay admin-only".into());
    }
    let mut previous_final = None;
    if elevated {
        if !ensure_root(&path, notes, &mut previous_final) {
            return None;
        }
    } else {
        match open_dir(&path, false).and_then(|d| dir_facts(&d)) {
            Ok(f) if classify(&f, false) == Verdict::Use => {}
            Ok(f) => {
                notes.push(format!(
                    "data: {} is not trustworthy ({:?}): running without persistent data",
                    path.display(),
                    classify(&f, false)
                ));
                return None;
            }
            Err(e) => {
                notes.push(format!(
                    "data: {} not usable without elevation ({e}): running without persistent data",
                    path.display()
                ));
                return None;
            }
        }
    }
    let cache_ok = ensure_subdir(&path.join(CACHE_NAME), None, elevated, notes).is_some();
    let icons_sd = user_sid.as_deref().and_then(icons_sddl);
    let icons_final = ensure_subdir(&path.join(ICONS_NAME), icons_sd.as_deref(), elevated, notes);
    ensure_plumbing_dirs(&path, elevated, notes);
    Some(Root { path, writable: elevated, cache_ok, icons_final, user_sid, previous_final })
}

/// Elevated only: creates (or verifies) `suite\` and `journal\`, which inherit the root DACL like `cache\`. A process that is
/// not elevated never creates them (and says nothing: an older root simply has none yet).
fn ensure_plumbing_dirs(root: &Path, elevated: bool, notes: &mut Vec<String>) {
    if !elevated {
        return;
    }
    for name in [SUITE_NAME, JOURNAL_NAME] {
        let _ = ensure_subdir(&root.join(name), None, true, notes);
    }
}

/// Elevated: creates the root with the protected DACL or validates the existing one. A hostile
/// (squatted) root, or an admin-owned one with a loose DACL, is renamed away once through the
/// inspected handle, then recreated. For the loose admin-owned case `previous` receives the final
/// path of the moved folder (its config.json may be migrated).
fn ensure_root(path: &Path, notes: &mut Vec<String>, previous: &mut Option<String>) -> bool {
    let shown = path.display();
    for attempt in 0..2 {
        match create_dir(path, Some(&root_sddl())) {
            Ok(()) => notes.push(format!("data: created {shown} (admin-only)")),
            Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => {}
            Err(e) => {
                notes.push(format!("data: cannot create {shown} ({e}): running without persistent data"));
                return false;
            }
        }
        let dir = match open_dir(path, true) {
            Ok(d) => d,
            Err(e) => {
                notes.push(format!("data: cannot open {shown} ({e}): running without persistent data"));
                return false;
            }
        };
        let facts = match dir_facts(&dir) {
            Ok(f) => f,
            Err(e) => {
                notes.push(format!("data: cannot inspect {shown} ({e}): running without persistent data"));
                return false;
            }
        };
        match classify(&facts, true) {
            Verdict::Use => return true,
            Verdict::RepairDacl => {
                // Admin-owned but non-admins could write: anything inside may have been planted while
                // the DACL was loose, and fixing the root alone would not fix the children. Nothing
                // pre-existing is trusted: the whole folder is moved aside and recreated; only its
                // config.json may later be read back through the checked path (`read_previous_config`).
                if attempt == 0 {
                    match rename_away(&dir, path, "old") {
                        Ok(new_name) => {
                            *previous = final_path(handle_of(&dir.handle));
                            notes.push(format!(
                                "data: {shown} let non-admins write: moved to '{new_name}' (delete it once \
                                 you checked it), creating a fresh one"
                            ));
                            continue;
                        }
                        Err(e) => notes.push(format!("data: moving the loosely secured {shown} away failed: {e}")),
                    }
                }
                notes.push(format!(
                    "data: {shown} lets non-admins write: NOT used, running without persistent data. Delete it \
                     from an elevated prompt (rmdir /s /q \"{shown}\") and restart the dock."
                ));
                return false;
            }
            Verdict::Hostile => {
                if attempt == 0 {
                    match rename_away(&dir, path, "untrusted") {
                        Ok(new_name) => {
                            notes.push(format!(
                                "data: {shown} was not created by an administrator (possible squatting): \
                                 renamed to '{new_name}' (delete it), creating a fresh one"
                            ));
                            continue;
                        }
                        Err(e) => notes.push(format!("data: renaming the untrusted {shown} away failed: {e}")),
                    }
                }
                notes.push(format!(
                    "data: {shown} is not owned by Administrators/SYSTEM (possible squatting): NOT used, running \
                     without persistent data. Delete it from an elevated prompt (rmdir /s /q \"{shown}\") and \
                     restart the dock."
                ));
                return false;
            }
            Verdict::Refuse => {
                notes.push(format!(
                    "data: {shown} is a reparse point or not a folder: NOT used, running without persistent \
                     data. Delete it from an elevated prompt (rmdir \"{shown}\") and restart the dock."
                ));
                return false;
            }
        }
    }
    false
}

/// Creates/validates a subfolder of the (already validated) root. `sddl` = explicit DACL to apply
/// (always re-applied when elevated, so the user ACE follows the current user); `None` = inherit the
/// root DACL and require admin-only. Returns the folder's final path, `None` = not usable.
fn ensure_subdir(path: &Path, sddl: Option<&str>, elevated: bool, notes: &mut Vec<String>) -> Option<String> {
    let shown = path.display();
    if elevated {
        match create_dir(path, sddl) {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => {}
            Err(e) => {
                notes.push(format!("data: cannot create {shown}: {e}"));
                return None;
            }
        }
    }
    let dir = open_dir(path, elevated).map_err(|e| notes.push(format!("data: cannot open {shown}: {e}"))).ok()?;
    let mut facts = dir_facts(&dir).map_err(|e| notes.push(format!("data: cannot inspect {shown}: {e}"))).ok()?;
    if sddl.is_some() {
        // The user ACE is expected here; the DACL is re-applied below anyway.
        facts.admin_only = true;
    }
    match classify(&facts, elevated) {
        Verdict::Use => {}
        Verdict::RepairDacl => match set_dacl(&dir, &root_sddl()) {
            Ok(()) => notes.push(format!("data: {shown} let non-admins write: protected DACL re-applied")),
            Err(e) => {
                notes.push(format!("data: {shown} lets non-admins write and could not be fixed ({e}): not used"));
                return None;
            }
        },
        v => {
            notes.push(format!("data: {shown} is not usable ({v:?}): not used"));
            return None;
        }
    }
    if let (true, Some(s)) = (elevated, sddl) {
        if let Err(e) = set_dacl(&dir, s) {
            notes.push(format!("data: applying the DACL of {shown} failed ({e}): you may not be able to add files"));
        }
    }
    final_path(handle_of(&dir.handle))
}

// ------------------------------------------------------------------ file I/O

/// Reads the custom icon `<root>\icons\<name>` (the user can create entries there): plain name only,
/// opened without following reparse points, single-link regular file, final path exactly
/// `<icons>\<name>`, at most `ICON_MAX_BYTES`. The bytes are decoded by the caller from memory.
pub fn read_icon(name: &str) -> io::Result<Vec<u8>> {
    if !crate::icons::is_plain_icon_name(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a plain file name"));
    }
    let r = root_ref().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no data folder"))?;
    let dir_final =
        r.icons_final.as_deref().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no icons folder"))?;
    let f = open_checked(&r.path.join(ICONS_NAME).join(name), true)?;
    let fin = final_path(handle_of(&f)).ok_or_else(|| io::Error::other("final path unavailable"))?;
    if !final_path_matches(&fin, dir_final, name) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("resolves to {fin}, outside the icons folder"),
        ));
    }
    read_capped(f, ICON_MAX_BYTES)
}

// ------------------------------------------------------------------ update staging

/// The verified root, only if this process may write in it (elevated).
fn writable_root() -> Result<&'static Root, String> {
    root_ref().filter(|r| r.writable).ok_or_else(|| "no writable data folder".to_string())
}

/// Rejects anything but a plain tag and one of `STAGE_FILES`.
fn check_stage_names(tag: &str, name: Option<&str>) -> Result<(), String> {
    if !is_plain_dir_name(tag) {
        return Err(format!("{tag:?} is not a plain folder name"));
    }
    match name {
        Some(n) if !STAGE_FILES.contains(&n) => Err(format!("{n:?} is not a staging file")),
        _ => Ok(()),
    }
}

/// `<root>\update`, which must already exist and be admin-only.
fn checked_update_dir(r: &Root) -> Result<PathBuf, String> {
    let update = r.path.join(UPDATE_NAME);
    checked_dir(&update)?;
    Ok(update)
}

/// Creates (or validates) `<root>\update` and `<root>\update\<tag>` and returns the latter. Both
/// inherit the protected root DACL (no explicit SDDL) and must end up admin-only.
pub fn update_stage_dir(tag: &str) -> Result<PathBuf, String> {
    let r = writable_root()?;
    check_stage_names(tag, None)?;
    let mut notes = Vec::new();
    let update = r.path.join(UPDATE_NAME);
    let stage = update.join(tag);
    for dir in [&update, &stage] {
        if ensure_subdir(dir, None, true, &mut notes).is_none() {
            return Err(notes.join("; "));
        }
    }
    Ok(stage)
}

/// Names of the real subfolders of an EXISTING `<root>\update` that are plain names (never creates
/// it). Reparse points and odd names are skipped; empty without a writable root.
pub fn update_stage_names() -> Vec<String> {
    let Ok(update) = writable_root().and_then(checked_update_dir) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&update) else { return Vec::new() };
    entries
        .filter_map(Result::ok)
        .filter(|e| {
            // symlink_metadata never follows a junction or symlink.
            std::fs::symlink_metadata(e.path()).is_ok_and(|m| {
                use std::os::windows::fs::MetadataExt;
                m.is_dir() && m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
            })
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_plain_dir_name(n))
        .collect()
}

/// Writes `<root>\update\<tag>\<name>` atomically (admin-only, inherited DACL), creating the folders.
pub fn write_stage_file(tag: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
    writable_root()?;
    check_stage_names(tag, Some(name))?;
    let dir = update_stage_dir(tag)?;
    write_atomic(&dir.join(name), bytes, FileAcl::Inherit)
}

/// Copies `src` to `<root>\update\<tag>\<name>`: `CopyFileW` creates `<name>.tmp` new (the kernel copies the file, so
/// the caller's heap never holds it; the copy gets the stage folder's inherited admin-only DACL, as `FileAcl::Inherit`
/// does), then it is renamed over `<name>`. On an error the temp is deleted.
pub fn copy_into_stage(tag: &str, name: &str, src: &Path) -> Result<(), String> {
    writable_root()?;
    check_stage_names(tag, Some(name))?;
    let dest = update_stage_dir(tag)?.join(name);
    let mut tmp_name = dest.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    // A stale temp (crash): deleting a name removes the link itself, never a link target.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("removing {}: {e}", tmp.display())),
    }
    let (s, t, d) = (HSTRING::from(src.as_os_str()), HSTRING::from(tmp.as_os_str()), HSTRING::from(dest.as_os_str()));
    // SAFETY: the strings are NUL-terminated and live for the call; bFailIfExists = TRUE never writes through an
    // existing name or link.
    if let Err(e) = unsafe { CopyFileW(&s, &t, true) } {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("copying {} to {}: {e}", src.display(), tmp.display()));
    }
    // SAFETY: both strings are NUL-terminated and live for the call.
    if let Err(e) = unsafe { MoveFileExW(&t, &d, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("replacing {}: {e}", dest.display()));
    }
    Ok(())
}

/// `NotFound` when the stage folder (or `update\` above it) does not exist, i.e. nothing is staged
/// yet and the caller must download. The admin-only checks would report a missing folder as
/// PermissionDenied, which `verify_stage` treats as a failed verification: the first update of a
/// fresh install then never downloaded (1.4.0 and 1.4.1).
fn stage_missing(stage: &Path) -> Option<io::Error> {
    match std::fs::symlink_metadata(stage) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Some(io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", stage.display())))
        }
        _ => None,
    }
}

/// Opens a staged file like `read_icon`: the stage folder (and `update\`) must be admin-only real
/// folders, the file a single-link regular file (no reparse point) whose final path is exactly
/// `<stage folder>\<name>`.
fn open_stage_file(tag: &str, name: &str) -> io::Result<(File, PathBuf)> {
    let denied = |e: String| io::Error::new(io::ErrorKind::PermissionDenied, e);
    let r = writable_root().map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    check_stage_names(tag, Some(name)).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if let Some(e) = stage_missing(&r.path.join(UPDATE_NAME).join(tag)) {
        return Err(e);
    }
    let dir = checked_update_dir(r).map_err(denied)?.join(tag);
    let dir_final = checked_dir(&dir).map_err(denied)?;
    let path = dir.join(name);
    let f = open_checked(&path, true)?;
    let fin = final_path(handle_of(&f)).ok_or_else(|| io::Error::other("final path unavailable"))?;
    if !final_path_matches(&fin, &dir_final, name) {
        return Err(denied(format!("resolves to {fin}, outside the staging folder")));
    }
    Ok((f, path))
}

/// Reads a staged file (at most `cap` bytes) through the checks of `open_stage_file`.
pub fn read_stage_file(tag: &str, name: &str, cap: u64) -> io::Result<Vec<u8>> {
    read_capped(open_stage_file(tag, name)?.0, cap)
}

/// Path of a staged file that passed the checks of `open_stage_file`: the only path the updater
/// may execute. The folder is admin-only, so it cannot be swapped after the check.
pub fn checked_stage_path(tag: &str, name: &str) -> io::Result<PathBuf> {
    open_stage_file(tag, name).map(|(_, p)| p)
}

/// Removes `<root>\update\<tag>` and everything in it (missing = Ok). `remove_dir_all` does not
/// follow junctions or symlinks inside the tree (Rust 1.58.1+); `update\` and the stage folder
/// themselves must be admin-only real folders.
pub fn remove_update_stage(tag: &str) -> Result<(), String> {
    let r = writable_root()?;
    check_stage_names(tag, None)?;
    let stage = r.path.join(UPDATE_NAME).join(tag);
    if std::fs::symlink_metadata(&stage).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
        return Ok(());
    }
    checked_update_dir(r)?;
    checked_dir(&stage)?;
    std::fs::remove_dir_all(&stage).map_err(|e| format!("removing {}: {e}", stage.display()))
}

/// Removes the whole `<root>\update` (missing = Ok), with the same guarantees as `remove_update_stage`.
pub fn remove_update_dir() -> Result<(), String> {
    let r = writable_root()?;
    let update = r.path.join(UPDATE_NAME);
    if std::fs::symlink_metadata(&update).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
        return Ok(());
    }
    checked_update_dir(r)?;
    std::fs::remove_dir_all(&update).map_err(|e| format!("removing {}: {e}", update.display()))
}

/// Deletes the whole data root (`--uninstall --remove --purge-data`). `remove_dir_all` does not follow
/// junctions or symlinks inside the tree (Rust 1.58.1+), which matters because `icons\` is
/// user-writable. Call only after `logger::stop`: `dock.log` is open while the logger runs.
pub fn purge() -> Result<(), String> {
    let r = writable_root()?;
    checked_dir(&r.path)?;
    std::fs::remove_dir_all(&r.path).map_err(|e| format!("removing {}: {e}", r.path.display()))
}

/// Reads the legacy `%LOCALAPPDATA%\Dock\config.json` for the one-time migration. `Ok(None)` = absent.
/// The folder is medium-writable, so: no reparse points, a single hard link, final path exactly
/// `<legacy dir>\config.json` (no redirected parent), owner = the token user or Administrators/SYSTEM
/// (older versions wrote it from the elevated process, so it is admin-owned; the user could edit it
/// anyway, and its content is parsed as untrusted), at most `CONFIG_MAX_BYTES`.
pub fn read_legacy_config() -> Result<Option<Vec<u8>>, String> {
    let Some(dir) = legacy_dir() else { return Ok(None) };
    let path = dir.join("config.json");
    let f = match open_checked(&path, true) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let fin = final_path(handle_of(&f)).unwrap_or_default();
    // Expected location in final-path form (`\\?\C:\...`), built from the known-folder path itself, NOT
    // from resolving the directory (which would follow a planted junction).
    let dir_s = dir.to_string_lossy();
    let expected_dir = if dir_s.starts_with(r"\\?\") { dir_s.into_owned() } else { format!(r"\\?\{dir_s}") };
    if !final_path_matches(&fin, &expected_dir, "config.json") {
        return Err(format!("{}: resolves to {fin}, skipped", path.display()));
    }
    let owner = read_security(handle_of(&f)).map_err(|e| format!("{}: {e}", path.display()))?.owner;
    let user = token_user_sid();
    let trusted = owner.is_some() && (owner == user || owner.as_deref().is_some_and(is_admin_sid));
    if !trusted {
        return Err(format!("{}: not owned by the current user or Administrators/SYSTEM", path.display()));
    }
    read_capped(f, CONFIG_MAX_BYTES).map(Some).map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads `config.json` from a data folder moved aside this run because its DACL was loose
/// (`ensure_root`). `Ok(None)` = no such folder or no config. Anything may have been planted in
/// it, so: no reparse points, a single hard link, final path exactly `<moved folder>\config.json`
/// (no redirected parent), owner Administrators/SYSTEM (a file the dock itself wrote; a planted one
/// is skipped), at most `CONFIG_MAX_BYTES`.
pub fn read_previous_config() -> Result<Option<Vec<u8>>, String> {
    let Some(prev) = root_ref().and_then(|r| r.previous_final.as_deref()) else { return Ok(None) };
    let path = PathBuf::from(prev).join("config.json");
    let f = match open_checked(&path, true) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let fin = final_path(handle_of(&f)).unwrap_or_default();
    if !final_path_matches(&fin, prev, "config.json") {
        return Err(format!("{}: resolves to {fin}, skipped", path.display()));
    }
    let owner = read_security(handle_of(&f)).map_err(|e| format!("{}: {e}", path.display()))?.owner;
    if !owner.as_deref().is_some_and(is_admin_sid) {
        return Err(format!("{}: not owned by Administrators/SYSTEM, skipped", path.display()));
    }
    read_capped(f, CONFIG_MAX_BYTES).map(Some).map_err(|e| format!("{}: {e}", path.display()))
}

// ------------------------------------------------------------------ Win32 helpers

/// `CreateDirectoryW` with the descriptor of `sddl` (or inherited ACEs if `None`). Atomic: the folder
/// never exists with a weaker DACL.
fn create_dir(path: &Path, sddl: Option<&str>) -> windows::core::Result<()> {
    let sd = match sddl.map(SecDesc::from_sddl).transpose() {
        Ok(sd) => sd,
        Err(e) => return Err(windows::core::Error::new(windows::core::HRESULT(0x8007_000Du32 as i32), e)),
    };
    let sa = sd.as_ref().map(|sd| SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.as_ptr().0,
        bInheritHandle: BOOL(0),
    });
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: `wide` is NUL-terminated; `sa` and the descriptor it points to outlive the call.
    unsafe { CreateDirectoryW(&wide, sa.as_ref().map(|s| s as *const SECURITY_ATTRIBUTES)) }
}

/// Replaces the DACL of an open folder (handle-based: `SetKernelObjectSecurity` never propagates to
/// children, so it cannot be steered through a planted link inside).
fn set_dacl(d: &OpenDir, sddl: &str) -> Result<(), String> {
    let sd = SecDesc::from_sddl(sddl)?;
    let h = handle_of(&d.handle);
    // SAFETY: valid handle (opened with WRITE_DAC when possible, otherwise the call fails) and a
    // valid descriptor that lives for the call.
    let r = unsafe {
        SetKernelObjectSecurity(h, DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, sd.as_ptr())
    };
    match r {
        Ok(()) => Ok(()),
        // The descriptor itself carries SE_DACL_PROTECTED ("D:P"): retry without the extra flag.
        // SAFETY: as above.
        Err(_) => {
            unsafe { SetKernelObjectSecurity(h, DACL_SECURITY_INFORMATION, sd.as_ptr()) }.map_err(|e| e.to_string())
        }
    }
}

/// Renames the inspected object (folder or reparse point) to `<ROOT_NAME>.<tag>-<pid>-<n>` in the
/// same parent as `path` (where it was opened), through its own handle: exactly what was inspected is
/// moved, nothing is followed, nothing is written into it. Fails if the target name exists.
/// The target must be a full path: kernel32 resolves a bare name against the current directory.
fn rename_away(d: &OpenDir, path: &Path, tag: &str) -> Result<String, String> {
    let parent = path.parent().ok_or_else(|| "no parent folder".to_string())?;
    // SAFETY: no preconditions.
    let pid = unsafe { GetCurrentProcessId() };
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.subsec_nanos()).unwrap_or(0);
    let name = format!("{ROOT_NAME}.{tag}-{pid}-{nanos}");
    let target = parent.join(&name);
    let wide: Vec<u16> = target.as_os_str().encode_wide().collect();
    let name_bytes = wide.len() * 2;
    let total = std::mem::size_of::<FILE_RENAME_INFO>() + name_bytes;
    // Zeroed, u64-aligned buffer: ReplaceIfExists = false, RootDirectory = NULL.
    let mut buf = vec![0u64; total.div_ceil(8)];
    let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `buf` is large enough for the header plus the name (the trailing NUL comes from the
    // zeroed `FileName[1]` slot); writes stay inside it.
    unsafe {
        (*info).FileNameLength = name_bytes as u32;
        std::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            wide.len(),
        );
    }
    // SAFETY: handle opened with DELETE (otherwise the call fails); buffer valid for `total` bytes.
    unsafe { SetFileInformationByHandle(handle_of(&d.handle), FileRenameInfo, info.cast_const().cast(), total as u32) }
        .map_err(|e| e.to_string())?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_sys::datadir::{config_sddl, SID_ADMINISTRATORS, SID_SYSTEM};
    use windows::Win32::Storage::FileSystem::WRITE_DAC;

    #[test]
    fn icons_masks_match_the_documented_values() {
        // The icons folder ACE: add file but no add-subfolder (0x4), delete-child (0x40) or WRITE_DAC.
        assert_ne!(MASK_ICONS_DIR_USER & 0x2, 0);
        assert_eq!(MASK_ICONS_DIR_USER & (0x4 | 0x40 | DELETE.0 | WRITE_DAC.0), 0);
        assert_eq!(MASK_ICONS_FILE_USER & WRITE_DAC.0, 0);
    }

    #[test]
    fn root_and_icons_sddl_strings() {
        assert_eq!(root_sddl(), "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)");
        let sid = "S-1-5-21-1-2-3-1001";
        let icons = icons_sddl(sid).unwrap_or_default();
        assert!(icons.starts_with("O:BAD:P(A;OICI;FA;;;SY)"));
        assert!(icons.contains("(A;;0x1200ab;;;S-1-5-21-1-2-3-1001)"));
        assert!(icons.contains("(A;OIIO;0x13019f;;;S-1-5-21-1-2-3-1001)"));
        assert!(icons_sddl("WD").is_none());
    }

    #[test]
    fn sddl_strings_parse() {
        let sid = "S-1-5-21-1-2-3-1001";
        for s in [Some(root_sddl()), config_sddl(sid), icons_sddl(sid)] {
            let s = s.unwrap_or_default();
            assert!(SecDesc::from_sddl(&s).is_ok(), "{s}");
        }
    }

    #[test]
    fn stage_files_are_the_updater_files_only() {
        for n in [
            "dock.exe",
            "dock.exe.sha256",
            "dock-guard.exe",
            "rolled-back",
            "suite.json",
            "tasks.exe",
            "tasks-admin.exe",
            "files.exe",
            "lang-it.lang",
            "bad-tasks",
            "bad-tasks-admin",
            "bad-files",
            "bad-lang-it",
        ] {
            assert!(check_stage_names("v1.5.1", Some(n)).is_ok(), "{n}");
        }
        assert_eq!(STAGE_FILES.len(), 13);
        for n in ["dock-guard.exe.bak", "x.exe", "", "suite.json.tmp", "bad-dock", "lang-it.lang.new", "tasks.prev.exe"]
        {
            assert!(check_stage_names("v1.5.1", Some(n)).is_err(), "{n:?}");
        }
        assert!(STAGE_FILES.contains(&crate::update_release::GUARD_FILE));
        assert!(STAGE_FILES.contains(&crate::update_release::ROLLED_BACK_FILE));
    }

    #[test]
    fn user_cache_sddl_has_no_users_ace() {
        let sid = "S-1-5-21-1-2-3-1001";
        let s = user_cache_sddl(sid).unwrap_or_default();
        assert_eq!(s, "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;S-1-5-21-1-2-3-1001)");
        assert!(!s.contains(";BU)") && !s.contains("S-1-5-32-545"), "no BUILTIN\\Users ACE: {s}");
        assert!(SecDesc::from_sddl(&s).is_ok(), "{s}");
        // Neither an SDDL injection nor a path is ever made of something that is not a SID.
        for bad in ["S-1-5)(A;OICI;FA;;;WD", "WD", "", "S-1-", "..", "S-1-5-21-1\\..\\x", "s-1-5-18"] {
            assert!(user_cache_sddl(bad).is_none(), "{bad:?}");
            assert!(user_cache_dir(bad).is_err(), "{bad:?}");
        }
        // Unit tests never call init(): nothing is created.
        assert!(user_cache_dir(sid).is_err());
    }

    #[test]
    fn staging_needs_an_initialized_writable_root() {
        // Unit tests never call init(): nothing may be created or touched.
        assert!(update_stage_dir("v1.4.0").is_err());
        assert!(update_stage_names().is_empty());
        assert!(read_stage_file("v1.4.0", "dock.exe", 10).is_err());
        assert!(checked_stage_path("..", "dock.exe").is_err());
        assert!(write_stage_file("v1.4.0", "dock.exe", b"x").is_err());
        assert!(remove_update_stage("v1.4.0").is_err());
        assert!(remove_update_dir().is_err());
        assert!(purge().is_err());
    }

    #[test]
    fn copy_into_stage_refuses_without_a_data_folder() {
        // Unit tests never call init(): there is no stage folder to copy into, whatever the name.
        let src = std::env::current_exe().unwrap();
        assert!(copy_into_stage("v1.5.1", crate::update_release::GUARD_FILE, &src).is_err());
        assert!(copy_into_stage("v1.5.1", "x.exe", &src).is_err());
        assert!(update_stage_names().is_empty(), "nothing was created");
    }

    #[test]
    fn missing_stage_folder_is_not_found() {
        let base = std::env::temp_dir().join(format!("dock_stage_missing_{}", std::process::id()));
        let stage = base.join(UPDATE_NAME).join("v1.4.2");
        let kind = |p: &Path| stage_missing(p).map(|e| e.kind());
        // Fresh install: neither `update\` nor the tag folder exists.
        assert_eq!(kind(&stage), Some(io::ErrorKind::NotFound));
        // `update\` left by an earlier release, tag folder missing.
        assert!(std::fs::create_dir_all(stage.parent().unwrap_or(&base)).is_ok());
        assert_eq!(kind(&stage), Some(io::ErrorKind::NotFound));
        // Present: the admin-only checks decide.
        assert!(std::fs::create_dir(&stage).is_ok());
        assert_eq!(kind(&stage), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn suite_and_journal_dirs_are_admin_only() {
        let base = std::env::temp_dir().join(format!("dock_datadir_layout_{}", std::process::id()));
        let elevated = crate::shell_thread::is_elevated();
        if elevated {
            // An admin-only root, as the real one: the new folders must inherit it and classify `Use`.
            assert!(create_dir(&base, Some(&root_sddl())).is_ok());
        } else {
            let _ = std::fs::create_dir_all(&base);
        }
        // Not elevated: nothing is created and nothing is logged (the MODES rule).
        let mut notes = Vec::new();
        ensure_plumbing_dirs(&base, false, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!base.join(SUITE_NAME).exists() && !base.join(JOURNAL_NAME).exists());
        if elevated {
            ensure_plumbing_dirs(&base, true, &mut notes);
            assert!(notes.is_empty(), "no repair on an admin-only root: {notes:?}");
            for name in [SUITE_NAME, JOURNAL_NAME] {
                let p = base.join(name);
                assert!(checked_dir(&p).is_ok(), "{name} classifies Use");
                let od = open_dir(&p, false).unwrap_or_else(|e| panic!("open: {e}"));
                let aces = read_security(handle_of(&od.handle)).ok().and_then(|s| s.dacl).unwrap_or_default();
                let sids: Vec<_> = aces.iter().filter_map(|a| a.sid.clone()).collect();
                let want = [SID_SYSTEM, SID_ADMINISTRATORS, "S-1-5-32-545"];
                assert_eq!(sids, want.map(String::from));
            }
        } else {
            println!("skipped: the test runner is not elevated (the creating half needs an admin-owned root)");
        }
        let _ = std::fs::remove_dir(base.join(SUITE_NAME));
        let _ = std::fs::remove_dir(base.join(JOURNAL_NAME));
        let _ = std::fs::remove_dir(&base);
    }

    #[test]
    fn plumbing_paths_are_under_the_root() {
        let root = Path::new(r"C:\ProgramData\mini-oled-dock");
        assert_eq!(safe_mode_in(root), root.join("safe-mode"));
        assert_eq!(update_lock_in(root), root.join("update").join("suite.lock"));
        assert_eq!(update_journal_in(root), root.join("update.jnl"));
        for p in [safe_mode_in(root), update_lock_in(root), update_journal_in(root)] {
            assert!(p.starts_with(root));
        }
    }

    #[test]
    fn nothing_before_init() {
        // Unit tests never call `init`: no path is handed out, nothing is created.
        assert!(config_path().is_none() && log_path().is_none() && cache_dir().is_none() && !writable());
        assert!(safe_mode_path().is_none() && update_lock_path().is_none() && update_journal_path().is_none());
        assert!(oled_sys::datadir::suite_dir().is_none() && oled_sys::datadir::journal_dir().is_none());
        assert!(read_icon("a.png").is_err());
    }

    /// Real NTFS round trip, unelevated: a folder created with an explicit DACL is read back ACE by
    /// ACE, re-secured through its handle and renamed away through its handle.
    #[test]
    fn real_folder_inspection_repair_and_rename() {
        let Some(me) = token_user_sid() else { panic!("no token user") };
        let base = std::env::temp_dir().join(format!("dock_datadir_sec_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&base);
        let d = base.join("probe");
        let full = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{me})");
        assert!(create_dir(&d, Some(&full)).is_ok());
        let od = open_dir(&d, true).unwrap_or_else(|e| panic!("open: {e}"));
        let f = dir_facts(&od).unwrap_or_else(|e| panic!("facts: {e}"));
        assert!(f.is_dir && !f.is_reparse && !f.admin_only);
        // Unelevated, the owner is the user: hostile from the elevated dock's point of view.
        assert_eq!(f.owner_trusted, Some(crate::shell_thread::is_elevated()));
        let aces = read_security(handle_of(&od.handle)).ok().and_then(|s| s.dacl).unwrap_or_default();
        let sids: Vec<_> = aces.iter().filter_map(|a| a.sid.clone()).collect();
        assert_eq!(sids, vec![SID_SYSTEM.to_string(), SID_ADMINISTRATORS.to_string(), me.clone()]);
        assert!(aces.iter().all(|a| !a.deny && a.mask == 0x1f01ff));
        // Read-only for the user: admin-only.
        assert!(set_dacl(&od, &format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;{me})")).is_ok());
        assert!(dir_facts(&od).is_ok_and(|f| f.admin_only));
        // Final path of a file inside matches `<dir final>\<name>`.
        let file = d.join("a.png");
        assert!(std::fs::write(&file, b"x").is_err() || std::fs::remove_file(&file).is_ok());
        assert!(set_dacl(&od, &full).is_ok());
        assert!(std::fs::write(&file, b"x").is_ok());
        let dir_final = final_path(handle_of(&od.handle)).unwrap_or_default();
        let fin = open_checked(&file, true).ok().and_then(|f| final_path(handle_of(&f))).unwrap_or_default();
        assert!(final_path_matches(&fin, &dir_final, "a.png"), "{fin} vs {dir_final}");
        let _ = std::fs::remove_file(&file);
        // Rename through the inspected handle (simple name = same parent).
        let new_name = rename_away(&od, &d, "untrusted").unwrap_or_else(|e| panic!("rename: {e}"));
        drop(od);
        assert!(new_name.starts_with("mini-oled-dock.untrusted-"));
        assert!(base.join(&new_name).is_dir() && std::fs::symlink_metadata(&d).is_err());
        let _ = std::fs::remove_dir(base.join(&new_name));
        let _ = std::fs::remove_dir(&base);
    }
}
