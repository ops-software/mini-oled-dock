//! The OS side of the change journal: registry, tasks, files and the locked journal file (F-P5).
//!
//! THE LOCK: a journal is written only through `open_locked` (or `open_locked_at`), which opens it for reading and
//! writing with share mode READ | DELETE. A second writer's open needs write access, which that share mode refuses, so
//! it waits (retrying every 20 ms up to its timeout, then "journal busy") until the holder drops its FileSink, while
//! readers (`load`: share READ | WRITE | DELETE) still read. The journal is parsed from the held handle, so the next seq
//! and run are read under the lock; every append ends with FlushFileBuffers; a torn tail left by a crash is cut before
//! the first append (so the next line never joins it), and an append that fails is cut back the same way.
//!
//! SCOPES: the user journal (%LOCALAPPDATA%\mini-oled-dock\journal\user.jsonl) is written only by a process that is
//! not elevated and never read by one that is; the machine journal (<root>\journal\machine.jsonl) is written only
//! elevated, in the admin-only journal folder of a writable data root. RealOs reaches only its scope's hive (User:
//! HKCU, Machine: HKLM; the other is refused before any call), always in the 64-bit view.
//!
//! TASKS: through schtasks.exe (no window), its XML staged in the scope's journal folder (admin-only for the machine
//! journal), as the dock stages autostart-task.xml. Task Scheduler stores what it registers in its own form, so the
//! XML read back is not byte for byte the XML written: the ownership check of a Task change compares those texts.
//!
//! THREADING: no state; every call blocks (schtasks.exe runs up to 20 s), so never on a UI thread.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use oled_trust::journal::{
    Digest, Hive, Journal, JournalError, OsErr, RegData, RegPath, RegSnapshot, Scope, TaskName, SNAPSHOT_DEPTH_MAX,
    SNAPSHOT_MAX,
};
use oled_trust::manifest::suite::COMPONENT_MAX_BYTES;
use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, ERROR_SUCCESS, GENERIC_READ, HANDLE, WIN32_ERROR,
};
use windows::Win32::Globalization::{MultiByteToWideChar, CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, FileDispositionInfo, FileDispositionInfoEx, GetFileAttributesW, MoveFileExW,
    ReOpenFile, SetFileInformationByHandle, DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_DATA, INVALID_FILE_ATTRIBUTES, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, OPEN_ALWAYS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegDeleteValueW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
    RegQueryInfoKeyW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    KEY_CREATE_SUB_KEY, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY,
    REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_VALUE_TYPE,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, CREATE_NO_WINDOW};
use windows::Win32::UI::Shell::FOLDERID_LocalAppData;

use crate::datadir;

/// The journal folder's name, under the data root and under %LOCALAPPDATA%\mini-oled-dock.
const JOURNAL_DIR: &str = "journal";
const USER_FILE: &str = "user.jsonl";
const MACHINE_FILE: &str = "machine.jsonl";
const UPDATE_FILE: &str = "update.jnl";
/// A journal file is read whole, at most this much.
const LOAD_MAX: u64 = 4 * 1024 * 1024;
/// The longest line of a record that closes entries (Undone, RunFailed, Committed: at most 55 bytes, u64::MAX numbers
/// included); every Entry line is longer (the shortest one an encoder can write takes 80 bytes).
const CLOSE_LINE_MAX: usize = 64;
/// Entry lines stop this far below LOAD_MAX: room for MAX_RECORDS closing lines, so an undo can always close every
/// entry and the file never grows past what open_locked reads back.
const CLOSE_RESERVE: u64 = (oled_trust::journal::MAX_RECORDS * CLOSE_LINE_MAX) as u64;
/// How often a waiting writer tries the lock again.
const LOCK_RETRY: Duration = Duration::from_millis(20);
/// schtasks.exe gets this long, as autostart.rs gives it.
const SCHTASKS_TIMEOUT: Duration = Duration::from_secs(20);
/// What a task query may print (a task's XML is a few KiB; the journal keeps at most 32 KiB of it).
const SCHTASKS_OUT_MAX: u64 = 256 * 1024;
/// The raw bytes a registry tree may hold before its snapshot certainly encodes over SNAPSHOT_MAX (a UTF-16 unit
/// encodes to at least one byte, a data byte to two hex digits).
const TREE_RAW_MAX: usize = 4 * SNAPSHOT_MAX;
/// One registry value larger than a journal line can never be journaled: it is not read.
const VALUE_MAX: usize = oled_trust::journal::MAX_LINE;
/// How deep a tree is walked when it is deleted (our own keys are at most SNAPSHOT_DEPTH_MAX deep).
const DELETE_DEPTH_MAX: u32 = 64;

const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_BINARY: u32 = 3;
const REG_DWORD: u32 = 4;
const REG_MULTI_SZ: u32 = 7;
const REG_QWORD: u32 = 11;

const CODE_ACCESS_DENIED: u32 = 5;
const CODE_PATH_NOT_FOUND: u32 = 3;
const CODE_INVALID_DATA: u32 = 13;
const CODE_INVALID_PARAMETER: u32 = 87;
const CODE_FILE_TOO_LARGE: u32 = 223;
const CODE_FILE_CORRUPT: u32 = 1392;
const CODE_IO_DEVICE: u32 = 1117;
const CODE_DISK_FULL: u32 = 112;

fn err(code: u32, what: &str) -> OsErr {
    OsErr { code, what: what.to_string() }
}

fn win32_err(what: &str, e: WIN32_ERROR) -> OsErr {
    OsErr { code: e.0, what: what.to_string() }
}

/// The Win32 code inside an HRESULT_FROM_WIN32, or the HRESULT itself.
fn hr_err(what: &str, e: &windows::core::Error) -> OsErr {
    let hr = e.code().0 as u32;
    let code = if hr & 0xFFFF_0000 == 0x8007_0000 { hr & 0xFFFF } else { hr };
    OsErr { code, what: what.to_string() }
}

fn io_err(what: &str, e: &io::Error) -> OsErr {
    OsErr { code: e.raw_os_error().map_or(0, |c| c as u32), what: format!("{what}: {e}") }
}

/// The 32-bit "file corrupt" error for a journal that does not parse, with the reason (no path, no content).
fn journal_err(e: &JournalError) -> OsErr {
    let what = match e {
        JournalError::Corrupt { line } => format!("journal: corrupt at line {line}"),
        JournalError::TooLarge => format!("journal: more than {} records", oled_trust::journal::MAX_RECORDS),
        JournalError::Version(v) => format!("journal: written by a newer version (schema {v})"),
        JournalError::BadPath | JournalError::BadName => "journal: a bad record".to_string(),
    };
    OsErr { code: CODE_FILE_CORRUPT, what }
}

// ------------------------------------------------------------------------------------------------ the registry

pub struct RealOs {
    scope: Scope,
}
impl RealOs {
    pub fn for_scope(scope: oled_trust::journal::Scope) -> RealOs {
        RealOs { scope }
    }

    /// The one hive this scope reaches; the other is refused here, before any registry call.
    fn root(&self, hive: Hive) -> Result<HKEY, OsErr> {
        match (self.scope, hive) {
            (Scope::User, Hive::Hkcu) => Ok(HKEY_CURRENT_USER),
            (Scope::Machine, Hive::Hklm) => Ok(HKEY_LOCAL_MACHINE),
            _ => Err(err(CODE_ACCESS_DENIED, "registry: the hive is outside the journal's scope")),
        }
    }

    /// Where task XML and schtasks output are staged: the scope's journal folder.
    fn stage_dir(&self) -> Result<PathBuf, OsErr> {
        match self.scope {
            Scope::User => user_dir_ready(),
            Scope::Machine => {
                datadir::journal_dir().ok_or_else(|| err(CODE_PATH_NOT_FOUND, "journal: no admin-only journal folder"))
            }
        }
    }
}
impl oled_trust::journal::JournalOs for RealOs {
    // Reg*W calls; tasks through schtasks.exe /Query /XML, /Create /XML /F, /Delete /F; digest = sha256
    fn reg_get(&self, hive: Hive, key: &RegPath, name: Option<&str>) -> Result<Option<RegData>, OsErr> {
        let root = self.root(hive)?;
        match open_key(root, key.as_str(), KEY_QUERY_VALUE)? {
            Some(k) => query_value(&k, name),
            None => Ok(None),
        }
    }
    fn reg_set(&mut self, hive: Hive, key: &RegPath, name: Option<&str>, data: Option<&RegData>) -> Result<(), OsErr> {
        let root = self.root(hive)?;
        let wide = name.map(HSTRING::from);
        let pname = wide.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
        match data {
            None => {
                let Some(k) = open_key(root, key.as_str(), KEY_SET_VALUE)? else { return Ok(()) };
                // SAFETY: `k` is an open key; `pname` is null (the default value) or points into `wide`, alive here.
                let rc = unsafe { RegDeleteValueW(k.0, pname) };
                if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
                    Ok(())
                } else {
                    Err(win32_err("RegDeleteValueW", rc))
                }
            }
            Some(d) => {
                let k = create_key(root, key.as_str(), KEY_SET_VALUE)?;
                let (ty, bytes) = to_reg(d);
                // SAFETY: `k` is an open key; `pname` as above; `bytes` is a live slice whose length the wrapper passes.
                let rc = unsafe { RegSetValueExW(k.0, pname, None, REG_VALUE_TYPE(ty), Some(&bytes)) };
                if rc == ERROR_SUCCESS {
                    Ok(())
                } else {
                    Err(win32_err("RegSetValueExW", rc))
                }
            }
        }
    }
    fn reg_key_exists(&self, hive: Hive, key: &RegPath) -> Result<bool, OsErr> {
        let root = self.root(hive)?;
        Ok(open_key(root, key.as_str(), KEY_QUERY_VALUE)?.is_some())
    }
    fn reg_key_create(&mut self, hive: Hive, key: &RegPath) -> Result<(), OsErr> {
        let root = self.root(hive)?;
        create_key(root, key.as_str(), KEY_QUERY_VALUE).map(drop)
    }
    fn reg_key_delete_if_empty(&mut self, hive: Hive, key: &RegPath) -> Result<bool, OsErr> {
        let root = self.root(hive)?;
        let Some(k) = open_key(root, key.as_str(), KEY_QUERY_VALUE)? else { return Ok(false) };
        let info = query_info(&k)?;
        drop(k);
        if info.subkeys != 0 || info.values != 0 {
            return Ok(false);
        }
        delete_key(root, key.as_str())
    }
    fn reg_tree_get(&self, hive: Hive, key: &RegPath) -> Result<Option<RegSnapshot>, OsErr> {
        let root = self.root(hive)?;
        let Some(k) = open_key(root, key.as_str(), KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS)? else { return Ok(None) };
        let mut budget = TREE_RAW_MAX;
        read_tree(&k, 1, &mut budget).map(Some)
    }
    fn reg_tree_put(&mut self, hive: Hive, key: &RegPath, snap: Option<&RegSnapshot>) -> Result<(), OsErr> {
        let root = self.root(hive)?;
        if let Some(s) = snap {
            check_names(s, 1)?;
        }
        delete_tree(root, key.as_str(), 0)?;
        match snap {
            Some(s) => write_tree(root, key.as_str(), s),
            None => Ok(()),
        }
    }
    fn task_xml(&self, name: &TaskName) -> Result<Option<String>, OsErr> {
        let dir = self.stage_dir()?;
        let out = dir.join(format!("task-{}.out", &crate::pipe::nonce()[..16]));
        let res = run_schtasks(&["/Query", "/TN", name.as_str(), "/XML"], Some(&out));
        let _ = std::fs::remove_file(&out);
        query_answer(res?, || task_file_absent(name))
    }
    fn task_put(&mut self, name: &TaskName, xml: Option<&str>) -> Result<(), OsErr> {
        let (args, staged) = match xml {
            Some(x) => {
                let file = self.stage_dir()?.join(format!("task-{}.xml", &crate::pipe::nonce()[..16]));
                write_new(&file, &schtasks_xml_bytes(x))?;
                (
                    vec![
                        "/Create".to_string(),
                        "/TN".into(),
                        name.as_str().into(),
                        "/XML".into(),
                        file.display().to_string(),
                        "/F".into(),
                    ],
                    Some(file),
                )
            }
            None => (vec!["/Delete".to_string(), "/TN".into(), name.as_str().into(), "/F".into()], None),
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let res = run_schtasks(&refs, None);
        if let Some(f) = staged {
            let _ = std::fs::remove_file(f);
        }
        match res?.0 {
            0 => Ok(()),
            code => Err(OsErr { code: code as u32, what: "schtasks /Create or /Delete failed".to_string() }),
        }
    }
    fn file_digest(&self, path: &str) -> Result<Option<Digest>, OsErr> {
        let f = match datadir::open_checked(Path::new(path), false) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err("opening a journaled file", &e)),
        };
        let bytes = datadir::read_capped(f, COMPONENT_MAX_BYTES).map_err(|e| io_err("reading a journaled file", &e))?;
        crate::sha256::sha256(&bytes).map(Some).map_err(|e| err(CODE_INVALID_DATA, &e))
    }
    fn file_delete(&mut self, path: &str) -> Result<(), OsErr> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err("deleting a journaled file", &e)),
        }
    }
    fn file_restore(&mut self, dest: &str, from: &str) -> Result<(), OsErr> {
        // A copy (the previous file stays, so a second undo finds dest equal to it) through a temp name, flushed, then
        // renamed over dest: dest is never half written.
        let tmp = format!("{dest}.restore");
        let _ = std::fs::remove_file(&tmp);
        std::fs::copy(from, &tmp).map_err(|e| io_err("copying the previous file", &e))?;
        let flushed = OpenOptions::new().write(true).open(&tmp).and_then(|f| f.sync_all());
        if let Err(e) = flushed {
            let _ = std::fs::remove_file(&tmp);
            return Err(io_err("flushing the restored file", &e));
        }
        let (src, dst) = (HSTRING::from(tmp.as_str()), HSTRING::from(dest));
        // SAFETY: both strings are NUL-terminated and live for the call.
        let moved = unsafe { MoveFileExW(&src, &dst, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) };
        moved.map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            hr_err("replacing a journaled file", &e)
        })
    }
    fn digest(&self, bytes: &[u8]) -> Result<Digest, OsErr> {
        crate::sha256::sha256(bytes).map_err(|e| err(CODE_INVALID_DATA, &e))
    }
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by this module and is closed once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn open_key(parent: HKEY, path: &str, access: REG_SAM_FLAGS) -> Result<Option<Key>, OsErr> {
    let wide = HSTRING::from(path);
    let mut hk = HKEY(std::ptr::null_mut());
    // SAFETY: `wide` is NUL-terminated and lives for the call; `hk` receives the key, owned by Key below.
    let rc = unsafe { RegOpenKeyExW(parent, &wide, Some(0), access | KEY_WOW64_64KEY, &mut hk) };
    if rc == ERROR_SUCCESS {
        Ok(Some(Key(hk)))
    } else if rc == ERROR_FILE_NOT_FOUND || rc == ERROR_PATH_NOT_FOUND {
        Ok(None)
    } else {
        Err(win32_err("RegOpenKeyExW", rc))
    }
}

/// Opens a key, creating it and the missing keys on its way.
fn create_key(parent: HKEY, path: &str, access: REG_SAM_FLAGS) -> Result<Key, OsErr> {
    let wide = HSTRING::from(path);
    let mut hk = HKEY(std::ptr::null_mut());
    // SAFETY: `wide` is NUL-terminated and lives for the call; no class, no security attributes; `hk` receives the
    // key, owned by Key below.
    let rc = unsafe {
        RegCreateKeyExW(
            parent,
            &wide,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            access | KEY_WOW64_64KEY,
            None,
            &mut hk,
            None,
        )
    };
    if rc == ERROR_SUCCESS {
        Ok(Key(hk))
    } else {
        Err(win32_err("RegCreateKeyExW", rc))
    }
}

/// Deletes a key that has no subkeys; true if it was there.
fn delete_key(parent: HKEY, path: &str) -> Result<bool, OsErr> {
    let wide = HSTRING::from(path);
    // SAFETY: `wide` is NUL-terminated and lives for the call.
    let rc = unsafe { RegDeleteKeyExW(parent, &wide, KEY_WOW64_64KEY.0, None) };
    if rc == ERROR_SUCCESS {
        Ok(true)
    } else if rc == ERROR_FILE_NOT_FOUND || rc == ERROR_PATH_NOT_FOUND {
        Ok(false)
    } else {
        Err(win32_err("RegDeleteKeyExW", rc))
    }
}

struct KeyInfo {
    subkeys: u32,
    max_subkey: u32,
    values: u32,
    max_value_name: u32,
    max_value: u32,
}

fn query_info(k: &Key) -> Result<KeyInfo, OsErr> {
    let mut i = KeyInfo { subkeys: 0, max_subkey: 0, values: 0, max_value_name: 0, max_value: 0 };
    // SAFETY: `k` is an open key; every pointer is a live, writable u32 of this frame.
    let rc = unsafe {
        RegQueryInfoKeyW(
            k.0,
            None,
            None,
            None,
            Some(&mut i.subkeys),
            Some(&mut i.max_subkey),
            None,
            Some(&mut i.values),
            Some(&mut i.max_value_name),
            Some(&mut i.max_value),
            None,
            None,
        )
    };
    if rc == ERROR_SUCCESS {
        Ok(i)
    } else {
        Err(win32_err("RegQueryInfoKeyW", rc))
    }
}

fn query_value(k: &Key, name: Option<&str>) -> Result<Option<RegData>, OsErr> {
    let wide = name.map(HSTRING::from);
    let pname = wide.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
    for _ in 0..4 {
        let (mut ty, mut size) = (REG_VALUE_TYPE(0), 0u32);
        // SAFETY: `k` is open; `pname` is null or points into `wide`; size query only (no data buffer).
        let rc = unsafe { RegQueryValueExW(k.0, pname, None, Some(&mut ty), None, Some(&mut size)) };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != ERROR_SUCCESS {
            return Err(win32_err("RegQueryValueExW", rc));
        }
        if size as usize > VALUE_MAX {
            return Err(err(CODE_FILE_TOO_LARGE, "registry: a value larger than a journal line"));
        }
        let mut buf = vec![0u8; size as usize];
        let mut got = size;
        // SAFETY: as above; `buf` has `got` writable bytes and the call writes at most that many.
        let rc = unsafe { RegQueryValueExW(k.0, pname, None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut got)) };
        if rc == ERROR_MORE_DATA {
            continue; // it grew between the two calls
        }
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != ERROR_SUCCESS {
            return Err(win32_err("RegQueryValueExW", rc));
        }
        buf.truncate(got as usize);
        return Ok(Some(from_reg(ty.0, &buf)));
    }
    Err(err(ERROR_MORE_DATA.0, "registry: a value kept changing size"))
}

fn utf16_le(s: &str) -> impl Iterator<Item = u8> + '_ {
    s.encode_utf16().flat_map(u16::to_le_bytes)
}

/// The registry type and bytes of a value: strings UTF-16 LE with their NUL, MULTI_SZ with one more NUL.
fn to_reg(d: &RegData) -> (u32, Vec<u8>) {
    match d {
        RegData::Sz(s) => (REG_SZ, utf16_le(s).chain([0, 0]).collect()),
        RegData::ExpandSz(s) => (REG_EXPAND_SZ, utf16_le(s).chain([0, 0]).collect()),
        RegData::MultiSz(l) => {
            let mut b: Vec<u8> = l.iter().flat_map(|s| utf16_le(s).chain([0, 0])).collect();
            b.extend([0, 0]);
            (REG_MULTI_SZ, b)
        }
        RegData::Dword(n) => (REG_DWORD, n.to_le_bytes().to_vec()),
        RegData::Qword(n) => (REG_QWORD, n.to_le_bytes().to_vec()),
        RegData::Binary(b) => (REG_BINARY, b.clone()),
        RegData::Other { ty, bytes } => (*ty, bytes.clone()),
    }
}

/// A value as RegData: typed only when writing it back gives the same bytes (no lossy string, no missing NUL),
/// otherwise Other with the raw bytes, so an undo always puts back exactly what was there.
fn from_reg(ty: u32, bytes: &[u8]) -> RegData {
    let units = || -> Option<Vec<u16>> {
        if !bytes.len().is_multiple_of(2) {
            return None;
        }
        Some(bytes.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect())
    };
    let text = |u: &[u16]| String::from_utf16(u).ok();
    let decoded = match ty {
        REG_SZ | REG_EXPAND_SZ => units().and_then(|u| {
            let s = text(u.strip_suffix(&[0])?)?;
            Some(if ty == REG_SZ { RegData::Sz(s) } else { RegData::ExpandSz(s) })
        }),
        REG_MULTI_SZ => units().and_then(|u| {
            let body = u.strip_suffix(&[0])?;
            if body.is_empty() {
                return Some(RegData::MultiSz(Vec::new()));
            }
            let list: Option<Vec<String>> = body.strip_suffix(&[0])?.split(|&c| c == 0).map(text).collect();
            list.map(RegData::MultiSz)
        }),
        REG_DWORD => <[u8; 4]>::try_from(bytes).ok().map(|b| RegData::Dword(u32::from_le_bytes(b))),
        REG_QWORD => <[u8; 8]>::try_from(bytes).ok().map(|b| RegData::Qword(u64::from_le_bytes(b))),
        REG_BINARY => Some(RegData::Binary(bytes.to_vec())),
        _ => None,
    };
    match decoded {
        Some(d) if to_reg(&d) == (ty, bytes.to_vec()) => d,
        _ => RegData::Other { ty, bytes: bytes.to_vec() },
    }
}

/// The names of a key's subkeys, as they are now.
fn subkey_names(k: &Key) -> Result<Vec<String>, OsErr> {
    let mut names = Vec::new();
    let mut cap = query_info(k)?.max_subkey as usize + 1;
    let mut i = 0u32;
    let mut retries = 0;
    loop {
        let mut buf = vec![0u16; cap];
        let mut len = cap as u32;
        // SAFETY: `k` is open; `buf` has `len` writable UTF-16 units; no class or time is asked for.
        let rc = unsafe { RegEnumKeyExW(k.0, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
        if rc == ERROR_NO_MORE_ITEMS {
            return Ok(names);
        }
        if rc == ERROR_MORE_DATA && retries < 4 {
            retries += 1;
            cap = query_info(k)?.max_subkey as usize + 1;
            continue;
        }
        if rc != ERROR_SUCCESS {
            return Err(win32_err("RegEnumKeyExW", rc));
        }
        let name = buf.get(..len as usize).and_then(|u| String::from_utf16(u).ok());
        names.push(name.ok_or_else(|| err(CODE_INVALID_DATA, "registry: a key name that is not valid UTF-16"))?);
        i += 1;
    }
}

/// A snapshot of an open key: values None first then by name, subkeys by name (the journal's canonical order).
fn read_tree(k: &Key, level: u8, budget: &mut usize) -> Result<RegSnapshot, OsErr> {
    let too_large = || err(CODE_FILE_TOO_LARGE, "registry: a tree larger than the journal's snapshot cap");
    if level > SNAPSHOT_DEPTH_MAX {
        return Err(too_large());
    }
    let mut info = query_info(k)?;
    let mut values = Vec::new();
    let mut i = 0u32;
    let mut retries = 0;
    loop {
        if info.max_value as usize > *budget {
            return Err(too_large());
        }
        let mut name = vec![0u16; info.max_value_name as usize + 1];
        let mut name_len = name.len() as u32;
        let mut data = vec![0u8; info.max_value as usize];
        let mut data_len = info.max_value;
        let mut ty = 0u32;
        // SAFETY: `k` is open; `name` has `name_len` writable units and `data` `data_len` writable bytes.
        let rc = unsafe {
            RegEnumValueW(
                k.0,
                i,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut ty),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        if rc == ERROR_NO_MORE_ITEMS {
            break;
        }
        if rc == ERROR_MORE_DATA && retries < 4 {
            retries += 1;
            info = query_info(k)?; // a value grew meanwhile
            continue;
        }
        if rc != ERROR_SUCCESS {
            return Err(win32_err("RegEnumValueW", rc));
        }
        let cost = 2 * name_len as usize + data_len as usize;
        *budget = budget.checked_sub(cost).ok_or_else(too_large)?;
        let n = name.get(..name_len as usize).and_then(|u| String::from_utf16(u).ok());
        let n = n.ok_or_else(|| err(CODE_INVALID_DATA, "registry: a value name that is not valid UTF-16"))?;
        let bytes = data.get(..data_len as usize).unwrap_or_default();
        values.push(((!n.is_empty()).then_some(n), from_reg(ty, bytes)));
        i += 1;
    }
    let mut subkeys = Vec::new();
    for name in subkey_names(k)? {
        *budget = budget.checked_sub(2 * name.len()).ok_or_else(too_large)?;
        // A subkey deleted since it was listed is simply not there.
        if let Some(sub) = open_key(k.0, &name, KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS)? {
            subkeys.push((name, read_tree(&sub, level + 1, budget)?));
        }
    }
    values.sort_by(|a, b| a.0.cmp(&b.0));
    subkeys.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(RegSnapshot { values, subkeys })
}

/// A tree to write: at most SNAPSHOT_DEPTH_MAX levels, subkey names that are one plain segment.
fn check_names(s: &RegSnapshot, level: u8) -> Result<(), OsErr> {
    let bad = || err(CODE_INVALID_PARAMETER, "registry: a tree with a bad subkey name or too deep");
    if level > SNAPSHOT_DEPTH_MAX {
        return Err(bad());
    }
    for (n, t) in &s.subkeys {
        if n.is_empty() || n.contains(['\\', '\0']) {
            return Err(bad());
        }
        check_names(t, level + 1)?;
    }
    Ok(())
}

/// Deletes a key and everything under it (no Reg*Tree helper: a walk), if it exists.
fn delete_tree(parent: HKEY, path: &str, depth: u32) -> Result<(), OsErr> {
    if depth > DELETE_DEPTH_MAX {
        return Err(err(CODE_INVALID_PARAMETER, "registry: a tree too deep to delete"));
    }
    let Some(k) = open_key(parent, path, KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS)? else { return Ok(()) };
    for child in subkey_names(&k)? {
        delete_tree(k.0, &child, depth + 1)?;
    }
    drop(k);
    delete_key(parent, path).map(drop)
}

fn write_tree(root: HKEY, path: &str, s: &RegSnapshot) -> Result<(), OsErr> {
    let k = create_key(root, path, KEY_SET_VALUE | KEY_CREATE_SUB_KEY)?;
    for (name, data) in &s.values {
        let wide = name.as_deref().map(HSTRING::from);
        let pname = wide.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
        let (ty, bytes) = to_reg(data);
        // SAFETY: `k` is open; `pname` is null or points into `wide`; `bytes` is a live slice.
        let rc = unsafe { RegSetValueExW(k.0, pname, None, REG_VALUE_TYPE(ty), Some(&bytes)) };
        if rc != ERROR_SUCCESS {
            return Err(win32_err("RegSetValueExW", rc));
        }
    }
    drop(k);
    for (name, t) in &s.subkeys {
        write_tree(root, &format!("{path}\\{name}"), t)?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------ tasks

/// The XML schtasks /Create /XML reads: UTF-16 LE with a BOM.
fn schtasks_xml_bytes(xml: &str) -> Vec<u8> {
    [0xFF, 0xFE].into_iter().chain(utf16_le(xml)).collect()
}

/// What schtasks /Query /XML printed: UTF-16 with a BOM, UTF-8, or else the OEM code page a redirected console
/// program writes in; its "\r\r\n" line ends (a text-mode quirk of redirected output) become "\r\n".
fn decode_schtasks_output(bytes: &[u8]) -> String {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        String::from_utf16_lossy(&units)
    } else if let Ok(s) = std::str::from_utf8(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)) {
        s.to_string()
    } else {
        oem_to_string(bytes)
    };
    text.replace("\r\r\n", "\r\n")
}

fn oem_to_string(bytes: &[u8]) -> String {
    // SAFETY: a length query over a live slice (no output buffer).
    let n = unsafe { MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
    let mut units = vec![0u16; usize::try_from(n).unwrap_or(0)];
    // SAFETY: `units` has room for the `n` units the query reported.
    let got = unsafe { MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut units)) };
    String::from_utf16_lossy(units.get(..usize::try_from(got).unwrap_or(0)).unwrap_or_default())
}

fn system_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    // SAFETY: `buf` is writable for its whole length.
    let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    let dir =
        buf.get(..n).filter(|_| n > 0).map(String::from_utf16_lossy).unwrap_or_else(|| r"C:\Windows\System32".into());
    PathBuf::from(dir)
}

fn schtasks_path() -> PathBuf {
    system_dir().join("schtasks.exe")
}

/// What a /Query /XML run answers. schtasks.exe exits 1 for a task that does not exist and for any other failure
/// (access denied, the service down), so 1 reads as absent only when the task's own file is certainly missing: a
/// failed query read as absent would let apply overwrite a task without its XML and undo close an entry of ours.
fn query_answer(run: (i32, Vec<u8>), absent: impl FnOnce() -> bool) -> Result<Option<String>, OsErr> {
    match run {
        (0, bytes) => Ok(Some(decode_schtasks_output(&bytes))),
        (1, _) if absent() => Ok(None),
        (code, _) => Err(OsErr { code: code as u32, what: "schtasks /Query failed".to_string() }),
    }
}

/// True only when Task Scheduler certainly holds no task by this name. A task of the root folder (a TaskName has no
/// other) is stored as the file of its name in %SystemRoot%\System32\Tasks, so only ERROR_FILE_NOT_FOUND for that
/// file counts; a denied or failed lookup, or a name of dots, does not.
fn task_file_absent(name: &TaskName) -> bool {
    let leaf = name.as_str().strip_prefix('\\').unwrap_or(name.as_str());
    if leaf.chars().all(|c| c == '.') {
        return false;
    }
    let path = format!(r"\\?\{}", system_dir().join("Tasks").join(leaf).display());
    let wide = HSTRING::from(path.as_str());
    // SAFETY: `wide` is NUL-terminated and lives for the call; GetLastError reads that call's error right after it.
    unsafe { GetFileAttributesW(&wide) == INVALID_FILE_ATTRIBUTES && GetLastError() == ERROR_FILE_NOT_FOUND }
}

/// Runs schtasks.exe without a window, at most SCHTASKS_TIMEOUT; its output goes to `out` (a new file) when given.
/// Returns the exit code and the output.
fn run_schtasks(args: &[&str], out: Option<&Path>) -> Result<(i32, Vec<u8>), OsErr> {
    let sink = match out {
        Some(p) => Some(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(p)
                .map_err(|e| io_err("staging the schtasks output", &e))?,
        ),
        None => None,
    };
    let stdout = match &sink {
        Some(f) => {
            f.try_clone().map(std::process::Stdio::from).map_err(|e| io_err("staging the schtasks output", &e))?
        }
        None => std::process::Stdio::null(),
    };
    let mut child = std::process::Command::new(schtasks_path())
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| io_err("starting schtasks.exe", &e))?;
    let started = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) if started.elapsed() > SCHTASKS_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err(1460, "schtasks.exe is not responding (timeout)"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(io_err("waiting for schtasks.exe", &e)),
        }
    };
    let mut bytes = Vec::new();
    if let Some(mut f) = sink {
        f.seek(SeekFrom::Start(0)).map_err(|e| io_err("reading the schtasks output", &e))?;
        f.take(SCHTASKS_OUT_MAX).read_to_end(&mut bytes).map_err(|e| io_err("reading the schtasks output", &e))?;
    }
    Ok((code, bytes))
}

/// A new file (never through an existing name or link) with these bytes, flushed.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), OsErr> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .map_err(|e| io_err("staging the task XML", &e))?;
    f.write_all(bytes).and_then(|()| f.sync_all()).map_err(|e| io_err("staging the task XML", &e))
}

// ------------------------------------------------------------------------------------------------ the journal file

pub struct FileSink {
    file: File,
    /// The end of the last whole line: where the next append starts.
    end: u64,
    /// An append failed and its part could not be cut back: nothing more is appended through this handle.
    broken: bool,
}
impl oled_trust::journal::Sink for FileSink {
    fn append(&mut self, bytes: &[u8]) -> Result<(), oled_trust::journal::OsErr> {
        if self.broken {
            return Err(err(CODE_IO_DEVICE, "journal: an earlier append could not be cut back"));
        }
        let room = if bytes.len() <= CLOSE_LINE_MAX { LOAD_MAX } else { LOAD_MAX - CLOSE_RESERVE };
        if self.end.saturating_add(bytes.len() as u64) > room {
            return Err(err(CODE_DISK_FULL, "journal: full"));
        }
        let file = &mut self.file;
        let written =
            file.seek(SeekFrom::Start(self.end)).and_then(|_| file.write_all(bytes)).and_then(|()| file.sync_all());
        match written {
            Ok(()) => {
                self.end += bytes.len() as u64;
                Ok(())
            }
            Err(e) => {
                // A part of the line may be on disk: cut it, so the next append starts a line of its own.
                if file.set_len(self.end).and_then(|()| file.sync_all()).is_err() {
                    self.broken = true;
                }
                Err(io_err("appending to the journal", &e))
            }
        }
    }
}
impl FileSink {
    pub fn delete_if_settled(self, j: &oled_trust::journal::Journal) -> Result<bool, oled_trust::journal::OsErr> {
        if !j.is_settled() {
            return Ok(false);
        }
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        // SAFETY: our own open handle; ReOpenFile opens the same file object (no path lookup) with DELETE access. It
        // fails when another handle does not share delete: the file is then kept.
        let reopened =
            unsafe { ReOpenFile(datadir::handle_of(&self.file), DELETE.0, share, FILE_FLAGS_AND_ATTRIBUTES(0)) };
        let Ok(h) = reopened else { return Ok(false) };
        // SAFETY: a fresh handle, owned here and closed on drop.
        let h = unsafe { OwnedHandle::from_raw_handle(h.0) };
        let posix = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
                FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
            ),
        };
        // SAFETY: `h` is open with DELETE; the buffer is a live FILE_DISPOSITION_INFO_EX of the size passed. POSIX
        // semantics take the name away at once, even while a reader still has the file open.
        let done = unsafe {
            SetFileInformationByHandle(
                HANDLE(h.as_raw_handle()),
                FileDispositionInfoEx,
                (&posix as *const FILE_DISPOSITION_INFO_EX).cast::<c_void>(),
                std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
            )
        };
        if done.is_err() {
            let classic = FILE_DISPOSITION_INFO { DeleteFile: true };
            // SAFETY: as above, with the classic disposition (the name goes when the last handle closes).
            unsafe {
                SetFileInformationByHandle(
                    HANDLE(h.as_raw_handle()),
                    FileDispositionInfo,
                    (&classic as *const FILE_DISPOSITION_INFO).cast::<c_void>(),
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            }
            .map_err(|e| hr_err("deleting the journal", &e))?;
        }
        Ok(true)
    }
} // under the lock
pub fn path(scope: oled_trust::journal::Scope) -> Option<std::path::PathBuf> {
    match scope {
        Scope::User => datadir::known_folder(&FOLDERID_LocalAppData)
            .map(|b| b.join(datadir::ROOT_NAME).join(JOURNAL_DIR).join(USER_FILE)),
        Scope::Machine => datadir::journal_dir().map(|d| d.join(MACHINE_FILE)),
    }
} // User: %LOCALAPPDATA%\mini-oled-dock\journal\user.jsonl; Machine: oled_sys::datadir::journal_dir()\machine.jsonl
pub fn update_journal_path() -> Option<std::path::PathBuf> {
    datadir::root().map(|r| r.path.join(UPDATE_FILE))
} // <root>\update.jnl (Scope::Machine format)
pub fn open_locked(
    scope: oled_trust::journal::Scope,
    timeout: std::time::Duration,
) -> Result<(oled_trust::journal::Journal, FileSink), oled_trust::journal::OsErr> {
    if !scope_allowed_now(scope, process_elevation()) {
        return Err(err(CODE_ACCESS_DENIED, "journal: this process may not write that journal"));
    }
    let file = match scope {
        Scope::User => user_dir_ready()?.join(USER_FILE),
        Scope::Machine => {
            if !datadir::writable() {
                return Err(err(CODE_ACCESS_DENIED, "journal: the data root is not writable"));
            }
            datadir::journal_dir()
                .ok_or_else(|| err(CODE_PATH_NOT_FOUND, "journal: no admin-only journal folder"))?
                .join(MACHINE_FILE)
        }
    };
    open_locked_at(&file, scope, timeout)
}
/// The lock on a journal file at a given path (the caller chose it: update.jnl, tests). Scope::User is refused to a
/// process that is elevated (or whose elevation cannot be read), as open_locked refuses it: an elevated undo of a file
/// a medium process writes would delete and restore files as admin. Scope::Machine is not checked here, so U1's
/// update.jnl tests run at medium.
pub fn open_locked_at(
    path: &std::path::Path,
    scope: oled_trust::journal::Scope,
    timeout: std::time::Duration,
) -> Result<(oled_trust::journal::Journal, FileSink), oled_trust::journal::OsErr> {
    if scope == Scope::User && !scope_allowed_now(Scope::User, process_elevation()) {
        return Err(err(CODE_ACCESS_DENIED, "journal: an elevated process never opens the user journal"));
    }
    let started = Instant::now();
    let mut file = loop {
        match open_for_lock(path) {
            Ok(f) => break f,
            Err(e) if e.code == ERROR_SHARING_VIOLATION.0 => {
                if started.elapsed() >= timeout {
                    return Err(err(ERROR_SHARING_VIOLATION.0, "journal busy"));
                }
                std::thread::sleep(
                    LOCK_RETRY.min(timeout.saturating_sub(started.elapsed())).max(Duration::from_millis(1)),
                );
            }
            Err(e) => return Err(e),
        }
    };
    let info = datadir::file_info(&file).map_err(|e| io_err("journal: reading its attributes", &e))?;
    datadir::check_regular(info.dwFileAttributes, info.nNumberOfLinks, true)
        .map_err(|why| err(CODE_ACCESS_DENIED, &format!("journal: {why}")))?;
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))
        .and_then(|_| (&mut file).take(LOAD_MAX + 1).read_to_end(&mut bytes))
        .map_err(|e| io_err("journal: reading it", &e))?;
    if bytes.len() as u64 > LOAD_MAX {
        return Err(err(CODE_FILE_TOO_LARGE, "journal: larger than 4 MiB"));
    }
    // A tail without its '\n' is an append a crash cut: it is cut here, so the next line does not join it.
    let cut = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
    if cut < bytes.len() {
        file.set_len(cut as u64)
            .and_then(|()| file.sync_all())
            .map_err(|e| io_err("journal: cutting a torn tail", &e))?;
        bytes.truncate(cut);
    }
    let j = Journal::parse(scope, &bytes).map_err(|e| journal_err(&e))?;
    Ok((j, FileSink { file, end: cut as u64, broken: false }))
}
pub fn load(scope: oled_trust::journal::Scope) -> Result<oled_trust::journal::Journal, String> {
    if scope == Scope::User && !scope_allowed_now(Scope::User, process_elevation()) {
        return Err("journal: an elevated process never reads the user journal".to_string());
    }
    let file = path(scope).ok_or_else(|| "journal: no journal folder".to_string())?;
    load_at(&file, scope)
} // read-only snapshot for lists only
pub fn scope_allowed(scope: oled_trust::journal::Scope, elevated: bool) -> bool {
    match scope {
        Scope::User => !elevated,
        Scope::Machine => elevated,
    }
} // User needs !elevated; Machine needs elevated

/// The write handle that is the lock: read and write (FILE_WRITE_DATA too, so a torn tail can be cut), shared with
/// readers and with deletion only; created when missing; a link at the path is opened as itself (and then refused).
fn open_for_lock(path: &Path) -> Result<File, OsErr> {
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: `wide` is NUL-terminated and lives for the call; OPEN_REPARSE_POINT never follows a link.
    let h = unsafe {
        CreateFileW(
            &wide,
            GENERIC_READ.0 | FILE_WRITE_DATA.0 | FILE_APPEND_DATA.0,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            None,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|e| hr_err("journal: opening it", &e))?;
    // SAFETY: a fresh valid handle, owned by nothing else.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(h.0) }))
}

/// A read-only snapshot of the journal at `file` (a missing file is an empty journal; a torn tail is ignored).
fn load_at(file: &Path, scope: Scope) -> Result<Journal, String> {
    let f = match datadir::open_checked(file, true) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Journal::empty(scope)),
        Err(e) => return Err(format!("journal: {e}")),
    };
    let bytes = datadir::read_capped(f, LOAD_MAX).map_err(|e| format!("journal: {e}"))?;
    Journal::parse(scope, &bytes).map_err(|e| journal_err(&e).what)
}

/// %LOCALAPPDATA%\mini-oled-dock\journal, created by the first writer (at medium), and a real folder (no junction
/// or link) at both levels.
fn user_dir_ready() -> Result<PathBuf, OsErr> {
    let base = datadir::known_folder(&FOLDERID_LocalAppData)
        .ok_or_else(|| err(CODE_PATH_NOT_FOUND, "journal: no local application data folder"))?;
    let app = base.join(datadir::ROOT_NAME);
    let dir = app.join(JOURNAL_DIR);
    for d in [&app, &dir] {
        make_real_dir(d)?;
    }
    Ok(dir)
}

fn make_real_dir(d: &Path) -> Result<(), OsErr> {
    let wide = HSTRING::from(d.as_os_str());
    // SAFETY: `wide` is NUL-terminated and lives for the call; the folder inherits its parent's DACL.
    if let Err(e) = unsafe { CreateDirectoryW(&wide, None) } {
        if e.code() != ERROR_ALREADY_EXISTS.to_hresult() {
            return Err(hr_err("journal: creating its folder", &e));
        }
    }
    let h = datadir::open_raw(d, FILE_READ_ATTRIBUTES.0).map_err(|e| hr_err("journal: opening its folder", &e))?;
    let attrs = datadir::file_info(&h).map_err(|e| io_err("journal: reading its folder", &e))?.dwFileAttributes;
    if attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0 || attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(err(CODE_ACCESS_DENIED, "journal: its folder is not a real folder"));
    }
    Ok(())
}

/// scope_allowed for this process: an elevation that cannot be read allows neither journal (both checks fail closed).
fn scope_allowed_now(scope: Scope, elevation: Option<bool>) -> bool {
    elevation.is_some_and(|e| scope_allowed(scope, e))
}

/// This process's elevation (its token's TokenElevation); None when the token cannot be read.
fn process_elevation() -> Option<bool> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle of this process; `token` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return None;
    }
    // SAFETY: a fresh valid token handle, closed when `_owned` drops.
    let _owned = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let mut e = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: `e` is a writable TOKEN_ELEVATION of the size passed; `len` is writable.
    let r = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut e as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    r.is_ok().then_some(e.TokenIsElevated != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_trust::journal::{
        apply, digest_tree, record, undo, AppId, Change, Entry, JournalOs, Kind, KindClass, Plan, Record, RunId, Sink,
        TakeoverId, UndoSet,
    };
    use std::process::{Command, Stdio};

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oled_journal_{tag}_{}", &crate::pipe::nonce()[..12]));
        std::fs::create_dir_all(&d).expect("a temp folder");
        d
    }

    fn kp(s: &str) -> RegPath {
        RegPath::new(s).expect("a valid test key")
    }

    fn file_change(i: u64) -> Change {
        Change::FileCreated { path: format!("C:\\made-up\\{i}.lnk"), after: [0; 32] }
    }

    #[test]
    fn paths() {
        let user = path(Scope::User).expect("a local application data folder");
        assert!(user.ends_with(Path::new("mini-oled-dock").join("journal").join("user.jsonl")), "{}", user.display());
        let base = datadir::known_folder(&FOLDERID_LocalAppData);
        assert_eq!(user.parent().and_then(Path::parent).and_then(Path::parent), base.as_deref());
        assert_eq!(path(Scope::Machine), datadir::journal_dir().map(|d| d.join("machine.jsonl")));
        assert_eq!(update_journal_path(), datadir::root().map(|r| r.path.join("update.jnl")));
    }

    #[test]
    fn scope_allowed_table() {
        assert!(scope_allowed(Scope::User, false));
        assert!(!scope_allowed(Scope::User, true));
        assert!(scope_allowed(Scope::Machine, true));
        assert!(!scope_allowed(Scope::Machine, false));
    }

    #[test]
    fn scope_refuses_the_other_hive() {
        // Every call is refused by the scope check, before the registry is reached.
        let mut user = RealOs::for_scope(Scope::User);
        let k = kp("SOFTWARE\\made-up");
        let code = |r: Result<(), OsErr>| r.err().map(|e| e.code);
        assert_eq!(code(user.reg_set(Hive::Hklm, &k, None, Some(&RegData::Dword(1)))), Some(5));
        assert_eq!(code(user.reg_key_create(Hive::Hklm, &k)), Some(5));
        assert_eq!(code(user.reg_tree_put(Hive::Hklm, &k, None)), Some(5));
        assert_eq!(user.reg_get(Hive::Hklm, &k, None).err().map(|e| e.code), Some(5));
        assert_eq!(user.reg_key_exists(Hive::Hklm, &k).err().map(|e| e.code), Some(5));
        assert_eq!(user.reg_tree_get(Hive::Hklm, &k).err().map(|e| e.code), Some(5));
        assert_eq!(user.reg_key_delete_if_empty(Hive::Hklm, &k).err().map(|e| e.code), Some(5));
        let mut machine = RealOs::for_scope(Scope::Machine);
        assert_eq!(code(machine.reg_set(Hive::Hkcu, &k, None, None)), Some(5));
        assert_eq!(machine.reg_get(Hive::Hkcu, &k, Some("v")).err().map(|e| e.code), Some(5));
        assert_eq!(code(machine.reg_tree_put(Hive::Hkcu, &k, None)), Some(5));
    }

    #[test]
    fn reg_data_conversion() {
        let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        let cases: Vec<(RegData, u32, Vec<u8>)> = vec![
            (RegData::Sz("ab".into()), 1, [utf16("ab"), vec![0, 0]].concat()),
            (RegData::Sz(String::new()), 1, vec![0, 0]),
            (RegData::ExpandSz("%x%\u{e8}".into()), 2, [utf16("%x%\u{e8}"), vec![0, 0]].concat()),
            (
                RegData::MultiSz(vec!["a".into(), "bc".into()]),
                7,
                [utf16("a"), vec![0, 0], utf16("bc"), vec![0, 0, 0, 0]].concat(),
            ),
            (RegData::MultiSz(vec![]), 7, vec![0, 0]),
            (RegData::MultiSz(vec![String::new()]), 7, vec![0, 0, 0, 0]),
            (RegData::Dword(0x0102_0304), 4, vec![4, 3, 2, 1]),
            (RegData::Qword(0x0102_0304_0506_0708), 11, vec![8, 7, 6, 5, 4, 3, 2, 1]),
            (RegData::Binary(vec![0, 255, 7]), 3, vec![0, 255, 7]),
            (RegData::Binary(vec![]), 3, vec![]),
        ];
        for (d, ty, bytes) in cases {
            assert_eq!(to_reg(&d), (ty, bytes.clone()), "{d:?}");
            assert_eq!(from_reg(ty, &bytes), d, "{d:?}");
        }
        // Anything a typed value would not write back byte for byte stays raw.
        let raw = |ty: u32, b: &[u8]| RegData::Other { ty, bytes: b.to_vec() };
        for (ty, b) in [
            (1u32, &[0x61u8, 0, 0x62][..]), // odd-length SZ
            (1, &[0x61, 0][..]),            // SZ without its NUL
            (2, &[0x00, 0xD8, 0, 0][..]),   // a lone surrogate
            (1, &[][..]),                   // empty SZ data
            (7, &[0x61, 0, 0, 0][..]),      // MULTI_SZ without its final NUL
            (4, &[1, 2, 3][..]),            // a 3-byte DWORD
            (11, &[1; 4][..]),              // a 4-byte QWORD
            (5, &[0, 0, 0, 1][..]),         // REG_DWORD_BIG_ENDIAN
            (0, &[9][..]),                  // REG_NONE
        ] {
            assert_eq!(from_reg(ty, b), raw(ty, b), "type {ty} {b:?}");
            assert_eq!(to_reg(&raw(ty, b)), (ty, b.to_vec()));
        }
        // Whatever the bytes, reading then writing gives them back.
        let mut x = 0x1234_5678_9abc_def0u64;
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let ty = [0u32, 1, 2, 3, 4, 7, 11][(x % 7) as usize];
            let bytes: Vec<u8> =
                (0..(x >> 8) % 12).map(|i| [0u8, 0x61, 0x00, 0xD8, 0xFF][((x >> (i * 3)) % 5) as usize]).collect();
            assert_eq!(to_reg(&from_reg(ty, &bytes)), (ty, bytes.clone()), "type {ty} {bytes:?}");
        }
    }

    #[test]
    fn schtasks_xml_is_utf16_with_bom() {
        let xml = "<Task>\u{e8}</Task>";
        let bytes = schtasks_xml_bytes(xml);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        assert_eq!(String::from_utf16(&units).ok().as_deref(), Some(xml));
        assert_eq!(decode_schtasks_output(&bytes), xml, "a UTF-16 answer");
        assert_eq!(
            decode_schtasks_output(b"<?xml?>\r\r\n<Task/>\r\r\n"),
            "<?xml?>\r\n<Task/>\r\n",
            "a redirected answer"
        );
        assert_eq!(decode_schtasks_output("\u{feff}<a>\u{e8}</a>".as_bytes()), "<a>\u{e8}</a>");
        assert_eq!(decode_schtasks_output(b"<a>x</a>"), "<a>x</a>");
        assert_eq!(decode_schtasks_output(b""), "");
    }

    // The temp-file tests use Scope::Machine (they journal FileCreated only): open_locked_at refuses Scope::User to a
    // test run that is elevated.

    #[test]
    fn a_second_writer_waits_then_times_out() {
        let dir = temp_dir("wait");
        let file = dir.join("j.jsonl");
        let (j, sink) = open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).expect("the first writer");
        let t = Instant::now();
        let busy = open_locked_at(&file, Scope::Machine, Duration::from_millis(100)).err();
        assert_eq!(busy, Some(OsErr { code: 32, what: "journal busy".into() }));
        assert!(t.elapsed() >= Duration::from_millis(100), "{:?}", t.elapsed());
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop((j, sink));
        });
        let t = Instant::now();
        let second = open_locked_at(&file, Scope::Machine, Duration::from_secs(2));
        assert!(second.is_ok(), "{:?}", second.err());
        assert!(t.elapsed() < Duration::from_secs(2));
        let _ = holder.join();
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn readers_read_while_a_writer_holds() {
        let dir = temp_dir("read");
        let file = dir.join("j.jsonl");
        let (mut j, mut sink) = open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).expect("the writer");
        let run = j.begin_run();
        assert_eq!(record(&mut j, &mut sink, run, Kind::UserEdit(AppId::Files), file_change(1), 5), Ok(1));
        let seen = load_at(&file, Scope::Machine).expect("a reader reads while the lock is held");
        assert_eq!(seen.records(), j.records());
        drop((j, sink));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_if_settled_only_under_the_lock() {
        let dir = temp_dir("delete");
        // Settled: the file is gone.
        let settled = dir.join("settled.jsonl");
        let (j, sink) = open_locked_at(&settled, Scope::Machine, Duration::from_secs(1)).expect("lock");
        assert_eq!(sink.delete_if_settled(&j), Ok(true));
        assert!(!settled.exists());
        // An open entry: kept.
        let open = dir.join("open.jsonl");
        let (mut j, mut sink) = open_locked_at(&open, Scope::Machine, Duration::from_secs(1)).expect("lock");
        let run = j.begin_run();
        assert!(record(&mut j, &mut sink, run, Kind::UserEdit(AppId::Dock), file_change(1), 0).is_ok());
        assert_eq!(sink.delete_if_settled(&j), Ok(false));
        assert_eq!(load_at(&open, Scope::Machine).map(|j| j.records().len()), Ok(1));
        // A reader that does not share delete: the delete fails softly and the file stays.
        let shared = dir.join("shared.jsonl");
        let (j, sink) = open_locked_at(&shared, Scope::Machine, Duration::from_secs(1)).expect("lock");
        let reader = OpenOptions::new().read(true).share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0).open(&shared);
        assert!(reader.is_ok(), "a reader sharing read and write opens beside the writer");
        assert_eq!(sink.delete_if_settled(&j), Ok(false));
        drop(reader);
        assert!(shared.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_torn_tail_is_cut_before_the_next_append() {
        let dir = temp_dir("torn");
        let file = dir.join("j.jsonl");
        let line = Journal::encode(&Record::Undone { seq: 1 }).expect("encodes");
        std::fs::write(&file, [line.clone(), line[..7].to_vec()].concat()).expect("a torn file");
        let (mut j, mut sink) = open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).expect("lock");
        assert_eq!(j.records(), &[Record::Undone { seq: 1 }]);
        let run = j.begin_run();
        assert_eq!(record(&mut j, &mut sink, run, Kind::UserEdit(AppId::Tasks), file_change(2), 0), Ok(2));
        drop((j, sink));
        let bytes = std::fs::read(&file).expect("read");
        assert!(bytes.ends_with(b"\n") && bytes.starts_with(&line));
        assert_eq!(load_at(&file, Scope::Machine).map(|j| j.records().len()), Ok(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_linked_or_corrupt_journal_is_refused() {
        let dir = temp_dir("refuse");
        let file = dir.join("j.jsonl");
        std::fs::write(&file, b"").expect("a file");
        let link = dir.join("link.jsonl");
        assert!(std::fs::hard_link(&file, &link).is_ok());
        assert_eq!(open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).err().map(|e| e.code), Some(5));
        assert!(load_at(&file, Scope::Machine).is_err());
        let _ = std::fs::remove_file(&link);
        std::fs::write(&file, b"00000000 {}\n").expect("a corrupt file");
        let e = open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).err();
        assert_eq!(e, Some(OsErr { code: 1392, what: "journal: corrupt at line 1".into() }));
        assert_eq!(load_at(&dir.join("missing.jsonl"), Scope::Machine).map(|j| j.records().len()), Ok(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The writer of two_writers_never_share_a_seq: this test exe, run with OLED_JOURNAL_TEST=append.
    fn spawn_writer(file: &Path) -> std::process::Child {
        let exe = std::env::current_exe().expect("the test exe");
        Command::new(exe)
            .args(["--exact", "journal_os::tests::journal_writer_child", "--ignored", "--test-threads=1"])
            .env("OLED_JOURNAL_TEST", "append")
            .env("OLED_JOURNAL_PATH", file)
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the writer starts")
    }

    #[test]
    fn two_writers_never_share_a_seq() {
        let dir = temp_dir("two");
        let file = dir.join("j.jsonl");
        let mut writers = [spawn_writer(&file), spawn_writer(&file)];
        let started = Instant::now();
        for w in &mut writers {
            let code = loop {
                match w.try_wait() {
                    Ok(Some(st)) => break st.code(),
                    Ok(None) if started.elapsed() > Duration::from_secs(180) => {
                        let _ = w.kill();
                        break None;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break None,
                }
            };
            assert_eq!(code, Some(0));
        }
        let j = load_at(&file, Scope::Machine).expect("the file parses");
        let mut seqs: Vec<u64> = j
            .records()
            .iter()
            .filter_map(|r| match r {
                Record::Entry(e) => Some(e.seq),
                _ => None,
            })
            .collect();
        assert_eq!(seqs.len(), 400, "no lost record");
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=400).collect::<Vec<u64>>(), "each seq once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "the writer process two_writers_never_share_a_seq starts (OLED_JOURNAL_TEST=append)"]
    fn journal_writer_child() {
        if std::env::var("OLED_JOURNAL_TEST").as_deref() != Ok("append") {
            return;
        }
        let Some(file) = std::env::var_os("OLED_JOURNAL_PATH").map(PathBuf::from) else { std::process::exit(2) };
        for i in 0..200 {
            let Ok((mut j, mut sink)) = open_locked_at(&file, Scope::Machine, Duration::from_secs(10)) else {
                std::process::exit(1)
            };
            let run = j.begin_run();
            if record(&mut j, &mut sink, run, Kind::UserEdit(AppId::Dock), file_change(i), i).is_err() {
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    #[test]
    fn the_user_journal_is_never_opened_elevated() {
        // An elevation that cannot be read allows neither journal.
        assert!(!scope_allowed_now(Scope::User, None) && !scope_allowed_now(Scope::Machine, None));
        assert!(scope_allowed_now(Scope::User, Some(false)) && scope_allowed_now(Scope::Machine, Some(true)));
        assert!(!scope_allowed_now(Scope::User, Some(true)) && !scope_allowed_now(Scope::Machine, Some(false)));
        // This test process, at whatever level it runs: Scope::User at a path only when it is known not elevated.
        let dir = temp_dir("scope");
        let file = dir.join("j.jsonl");
        let medium = process_elevation() == Some(false);
        let user = open_locked_at(&file, Scope::User, Duration::from_secs(1));
        assert_eq!(user.as_ref().err().map(|e| e.code), if medium { None } else { Some(5) });
        drop(user);
        assert!(open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).is_ok(), "Machine is not checked here");
        if !medium {
            assert!(load(Scope::User).is_err(), "refused before any read");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing exists and nothing may change: every journaled target reads as gone.
    struct AbsentOs;

    impl JournalOs for AbsentOs {
        fn reg_get(&self, _: Hive, _: &RegPath, _: Option<&str>) -> Result<Option<RegData>, OsErr> {
            Ok(None)
        }
        fn reg_set(&mut self, _: Hive, _: &RegPath, _: Option<&str>, _: Option<&RegData>) -> Result<(), OsErr> {
            Err(err(5, "absent"))
        }
        fn reg_key_exists(&self, _: Hive, _: &RegPath) -> Result<bool, OsErr> {
            Ok(false)
        }
        fn reg_key_create(&mut self, _: Hive, _: &RegPath) -> Result<(), OsErr> {
            Err(err(5, "absent"))
        }
        fn reg_key_delete_if_empty(&mut self, _: Hive, _: &RegPath) -> Result<bool, OsErr> {
            Err(err(5, "absent"))
        }
        fn reg_tree_get(&self, _: Hive, _: &RegPath) -> Result<Option<RegSnapshot>, OsErr> {
            Ok(None)
        }
        fn reg_tree_put(&mut self, _: Hive, _: &RegPath, _: Option<&RegSnapshot>) -> Result<(), OsErr> {
            Err(err(5, "absent"))
        }
        fn task_xml(&self, _: &TaskName) -> Result<Option<String>, OsErr> {
            Ok(None)
        }
        fn task_put(&mut self, _: &TaskName, _: Option<&str>) -> Result<(), OsErr> {
            Err(err(5, "absent"))
        }
        fn file_digest(&self, _: &str) -> Result<Option<Digest>, OsErr> {
            Ok(None)
        }
        fn file_delete(&mut self, _: &str) -> Result<(), OsErr> {
            Err(err(5, "absent"))
        }
        fn file_restore(&mut self, _: &str, _: &str) -> Result<(), OsErr> {
            Err(err(5, "absent"))
        }
        fn digest(&self, _: &[u8]) -> Result<Digest, OsErr> {
            Ok([0; 32])
        }
    }

    #[test]
    fn entries_stop_short_of_the_read_cap() {
        // Every closing line fits CLOSE_LINE_MAX; even a small entry line does not.
        let max = u64::MAX;
        for r in
            [Record::Undone { seq: max }, Record::RunFailed { run: RunId(max) }, Record::Committed { run: RunId(max) }]
        {
            assert!(Journal::encode(&r).expect("encodes").len() <= CLOSE_LINE_MAX, "{r:?}");
        }
        let change = Change::RegKeyCreated { hive: Hive::Hklm, key: kp("x") };
        let small = Record::Entry(Entry { seq: 1, run: RunId(1), ts: 0, kind: Kind::Install, change });
        assert!(Journal::encode(&small).expect("encodes").len() > CLOSE_LINE_MAX);
        // Trees of about 60 KiB until the sink refuses: the file stays readable, and undo still closes every entry.
        let dir = temp_dir("full");
        let file = dir.join("j.jsonl");
        let (mut j, mut sink) = open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).expect("lock");
        let big =
            RegSnapshot { values: vec![(Some("v".into()), RegData::Binary(vec![0xA5; 30 * 1024]))], subkeys: vec![] };
        let run = j.begin_run();
        let mut entries = 0usize;
        let refused = loop {
            let key = kp(&format!("SOFTWARE\\made-up\\{entries}"));
            let c = Change::RegTree { hive: Hive::Hklm, key, before: Some(big.clone()), after: [1; 32] };
            match record(&mut j, &mut sink, run, Kind::Install, c, 0) {
                Ok(_) => entries += 1,
                Err(e) => break e,
            }
            assert!(entries < 100, "never refused");
        };
        assert_eq!(refused, OsErr { code: 112, what: "journal: full".into() });
        assert!(entries > 50, "{entries}");
        drop((j, sink));
        let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(u64::MAX);
        assert!(size <= LOAD_MAX - CLOSE_RESERVE, "{size}");
        let (mut j, mut sink) = open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).expect("still opens");
        assert_eq!(j.open_entries().len(), entries);
        let report = undo(&mut j, &mut sink, &mut AbsentOs, &UndoSet::Classes(vec![KindClass::Install]));
        assert_eq!((report.changed_by_other.len(), report.failed.len()), (entries, 0));
        assert!(j.is_settled());
        drop((j, sink));
        let (j, _sink) = open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).expect("opens after the undo");
        assert!(j.is_settled());
        drop((j, _sink));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_task_query_is_absent_only_when_its_file_is_missing() {
        let q = |code: i32, absent: bool| query_answer((code, b"<Task/>".to_vec()), || absent).map_err(|e| e.code);
        assert_eq!(q(0, true), Ok(Some("<Task/>".into())));
        assert_eq!(q(1, true), Ok(None));
        assert_eq!(q(1, false), Err(1), "access denied or the service down: not read as absent");
        assert_eq!(q(5, true), Err(5));
        // The probe only reads: a made-up name has no task file; a name of dots is never confirmed absent.
        let made_up = TaskName::new(&format!("\\made-up {}", &crate::pipe::nonce()[..12])).expect("a valid name");
        assert!(task_file_absent(&made_up));
        assert!(!task_file_absent(&TaskName::new("\\..").expect("a valid name")));
    }

    // ---------------------------------------------------------------------------------------- the L-J1 drill

    /// RealOs that ends the process after `left` mutating calls: a crash between two OS operations.
    struct CrashingOs {
        inner: RealOs,
        left: usize,
    }

    impl CrashingOs {
        fn tick(&mut self) {
            if self.left == 0 {
                std::process::exit(9);
            }
            self.left -= 1;
        }
    }

    impl JournalOs for CrashingOs {
        fn reg_get(&self, h: Hive, k: &RegPath, n: Option<&str>) -> Result<Option<RegData>, OsErr> {
            self.inner.reg_get(h, k, n)
        }
        fn reg_set(&mut self, h: Hive, k: &RegPath, n: Option<&str>, d: Option<&RegData>) -> Result<(), OsErr> {
            self.tick();
            self.inner.reg_set(h, k, n, d)
        }
        fn reg_key_exists(&self, h: Hive, k: &RegPath) -> Result<bool, OsErr> {
            self.inner.reg_key_exists(h, k)
        }
        fn reg_key_create(&mut self, h: Hive, k: &RegPath) -> Result<(), OsErr> {
            self.tick();
            self.inner.reg_key_create(h, k)
        }
        fn reg_key_delete_if_empty(&mut self, h: Hive, k: &RegPath) -> Result<bool, OsErr> {
            self.tick();
            self.inner.reg_key_delete_if_empty(h, k)
        }
        fn reg_tree_get(&self, h: Hive, k: &RegPath) -> Result<Option<RegSnapshot>, OsErr> {
            self.inner.reg_tree_get(h, k)
        }
        fn reg_tree_put(&mut self, h: Hive, k: &RegPath, s: Option<&RegSnapshot>) -> Result<(), OsErr> {
            self.tick();
            self.inner.reg_tree_put(h, k, s)
        }
        fn task_xml(&self, n: &TaskName) -> Result<Option<String>, OsErr> {
            self.inner.task_xml(n)
        }
        fn task_put(&mut self, n: &TaskName, x: Option<&str>) -> Result<(), OsErr> {
            self.tick();
            self.inner.task_put(n, x)
        }
        fn file_digest(&self, p: &str) -> Result<Option<Digest>, OsErr> {
            self.inner.file_digest(p)
        }
        fn file_delete(&mut self, p: &str) -> Result<(), OsErr> {
            self.tick();
            self.inner.file_delete(p)
        }
        fn file_restore(&mut self, d: &str, f: &str) -> Result<(), OsErr> {
            self.tick();
            self.inner.file_restore(d, f)
        }
        fn digest(&self, b: &[u8]) -> Result<Digest, OsErr> {
            self.inner.digest(b)
        }
    }

    const DRILL_ROOT: &str = "Software\\mini-oled-dock-drill";

    /// The drill's key, its parent when empty and its temp folder go when the drill ends, a failed one included.
    struct DrillCleanup {
        key: String,
        dir: PathBuf,
    }

    impl Drop for DrillCleanup {
        fn drop(&mut self) {
            let mut os = RealOs::for_scope(Scope::User);
            if let Ok(key) = RegPath::new(&self.key) {
                let _ = os.reg_tree_put(Hive::Hkcu, &key, None);
            }
            let _ = os.reg_key_delete_if_empty(Hive::Hkcu, &kp(DRILL_ROOT));
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn drill_tree(tag: &str) -> RegSnapshot {
        let leaf =
            RegSnapshot { values: vec![(Some("z".into()), RegData::Binary(tag.as_bytes().to_vec()))], subkeys: vec![] };
        let mid = RegSnapshot {
            values: vec![(Some("w".into()), RegData::MultiSz(vec![tag.into()]))],
            subkeys: vec![("two".into(), leaf)],
        };
        RegSnapshot {
            values: vec![(None, RegData::Sz(tag.into())), (Some("v".into()), RegData::Dword(7))],
            subkeys: vec![("one".into(), mid)],
        }
    }

    /// Twelve changes: every REG type, two deletes, a three-level tree, a new key two levels deep and a value in it.
    fn drill_plan(os: &RealOs, base: &str) -> Plan {
        let v = |name: Option<&str>, after: Option<RegData>| Change::RegValue {
            hive: Hive::Hkcu,
            key: kp(base),
            name: name.map(str::to_string),
            before: None,
            after,
        };
        let tree = drill_tree("new");
        let after = digest_tree(&tree, os).expect("sha256");
        Plan {
            kind: Kind::Takeover(TakeoverId::FolderVerbs),
            changes: vec![
                v(Some("sz"), Some(RegData::Sz("drill".into()))),
                v(Some("expand"), Some(RegData::ExpandSz("%TEMP%\\d".into()))),
                v(Some("multi"), Some(RegData::MultiSz(vec!["a".into(), "b".into()]))),
                v(Some("dword"), Some(RegData::Dword(7))),
                v(Some("qword"), Some(RegData::Qword(1 << 40))),
                v(Some("binary"), Some(RegData::Binary(vec![1, 2, 3]))),
                v(Some("other"), Some(RegData::Other { ty: 0, bytes: vec![9, 9] })),
                v(Some("gone"), None),
                v(None, None),
                Change::RegTree { hive: Hive::Hkcu, key: kp(&format!("{base}\\tree")), before: Some(tree), after },
                Change::RegKeyCreated { hive: Hive::Hkcu, key: kp(&format!("{base}\\new\\deep")) },
                Change::RegValue {
                    hive: Hive::Hkcu,
                    key: kp(&format!("{base}\\new\\deep")),
                    name: Some("x".into()),
                    before: None,
                    after: Some(RegData::Dword(1)),
                },
            ],
        }
    }

    fn drill_child(k: usize) -> ! {
        let (Some(base), Some(journal)) =
            (std::env::var("OLED_DRILL_BASE").ok(), std::env::var_os("OLED_DRILL_JOURNAL"))
        else {
            std::process::exit(2)
        };
        let Ok((mut j, mut sink)) = open_locked_at(Path::new(&journal), Scope::User, Duration::from_secs(10)) else {
            std::process::exit(3)
        };
        let mut os = CrashingOs { inner: RealOs::for_scope(Scope::User), left: k };
        let plan = drill_plan(&os.inner, &base);
        let code = if apply(&mut j, &mut sink, &mut os, plan, 1).is_ok() { 0 } else { 4 };
        std::process::exit(code)
    }

    #[test]
    #[ignore = "L-J1 drill: writes under HKCU\\Software\\mini-oled-dock-drill; run only by the live tester"]
    fn journal_os_live_drill() {
        if let Some(k) = std::env::var("OLED_DRILL_CRASH_AT").ok().and_then(|k| k.parse().ok()) {
            drill_child(k);
        }
        let base = format!("{DRILL_ROOT}\\{}", &crate::pipe::nonce()[..16]);
        let dir = temp_dir("drill");
        let journal = dir.join("drill.jsonl");
        let _cleanup = DrillCleanup { key: base.clone(), dir: dir.clone() };
        let mut os = RealOs::for_scope(Scope::User);
        let key = kp(&base);
        os.reg_set(Hive::Hkcu, &key, Some("sz"), Some(&RegData::Sz("orig".into()))).expect("setup");
        os.reg_set(Hive::Hkcu, &key, Some("gone"), Some(&RegData::Dword(5))).expect("setup");
        os.reg_set(Hive::Hkcu, &key, None, Some(&RegData::Sz("default".into()))).expect("setup");
        os.reg_tree_put(Hive::Hkcu, &kp(&format!("{base}\\tree")), Some(&drill_tree("old"))).expect("setup");
        let original = os.reg_tree_get(Hive::Hkcu, &key).expect("snapshot");
        // The mutating calls of a whole run: k = 0..=that many covers a crash before each of them and none.
        let mut counting = CrashingOs { inner: RealOs::for_scope(Scope::User), left: usize::MAX };
        let mut probe = Journal::empty(Scope::User);
        let mut nothing = Vec::new();
        struct VecSink<'a>(&'a mut Vec<u8>);
        impl Sink for VecSink<'_> {
            fn append(&mut self, b: &[u8]) -> Result<(), OsErr> {
                self.0.extend_from_slice(b);
                Ok(())
            }
        }
        let run = apply(&mut probe, &mut VecSink(&mut nothing), &mut counting, drill_plan(&os, &base), 1)
            .expect("a whole run");
        let total = usize::MAX - counting.left;
        let _ = undo(&mut probe, &mut VecSink(&mut nothing), &mut os, &UndoSet::Run(run));
        assert_eq!(os.reg_tree_get(Hive::Hkcu, &key).expect("snapshot"), original, "a whole run undone");
        let mut empty = 0;
        for k in 0..=total {
            let _ = std::fs::remove_file(&journal);
            let exe = std::env::current_exe().expect("the test exe");
            let status = Command::new(exe)
                .args(["--exact", "journal_os::tests::journal_os_live_drill", "--ignored", "--test-threads=1"])
                .env("OLED_DRILL_CRASH_AT", k.to_string())
                .env("OLED_DRILL_BASE", &base)
                .env("OLED_DRILL_JOURNAL", &journal)
                .creation_flags(CREATE_NO_WINDOW.0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let (mut j, mut sink) =
                open_locked_at(&journal, Scope::User, Duration::from_secs(10)).expect("the journal");
            let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
            let now = os.reg_tree_get(Hive::Hkcu, &key).expect("snapshot");
            if now == original && report.failed.is_empty() && j.is_settled() {
                empty += 1;
            } else {
                println!("drill: k {k} (exit {:?}) left a diff: {report:?}", status.ok().and_then(|s| s.code()));
            }
            let _ = sink.delete_if_settled(&j);
        }
        println!("drill: {empty}/{} empty diffs", total + 1);
        assert_eq!(empty, total + 1);
    }
}
