//! The data-root mechanism shared by the dock and the apps: the checked, handle-based file I/O of the
//! admin-owned data folder `%ProgramData%\mini-oled-dock`, the access-mask and DACL decisions (pure), and the
//! root registry every consumer in a process reads.
//!
//! WHY: the dock runs elevated and its data folder is protected so that no medium-integrity process can plant
//! a link in it. Everything that reads or writes there opens its handles without following reparse points
//! and checks what it opened. The layout (names, caps, the creation SDDLs, `init`) stays with the dock; it
//! calls `register` once it has verified the root, so every consumer in the process sees the same root.
//! Apps use `open_existing`, which only accepts a root that is already safe as it is and never creates or
//! repairs anything.
//!
//! THREADING: everything is thread-safe (`OnceLock` state, no COM).

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::core::{BOOL, GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::{LocalFree, GENERIC_WRITE, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    AclSizeInformation, GetAce, GetAclInformation, GetKernelObjectSecurity, GetSecurityDescriptorDacl,
    GetSecurityDescriptorOwner, GetTokenInformation, TokenUser, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, GetFinalPathNameByHandleW, MoveFileExW, BY_HANDLE_FILE_INFORMATION,
    CREATE_NEW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GETFINALPATHNAMEBYHANDLE_FLAGS, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, OPEN_EXISTING, READ_CONTROL, VOLUME_NAME_DOS, WRITE_DAC,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// Folder name under `%ProgramData%`.
pub const ROOT_NAME: &str = "mini-oled-dock";

/// SIDs trusted as owner/writer of the data folders.
pub const SID_SYSTEM: &str = "S-1-5-18";

pub const SID_ADMINISTRATORS: &str = "S-1-5-32-544";

/// Read + execute (list/traverse for folders): `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE` (0x1200a9).
pub const MASK_READ_EXECUTE: u32 = FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0;

/// The user's ACE on `config.json`: read + write data, NOT delete, NOT WRITE_DAC (0x12019f).
pub const MASK_CONFIG_USER: u32 = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0;

/// Rights that let a trustee change a folder/file or its entries: write data / add file (0x2),
/// append / add subfolder (0x4), write EA (0x10), delete child (0x40), write attributes (0x100),
/// DELETE, WRITE_DAC, WRITE_OWNER, GENERIC_WRITE, GENERIC_ALL.
pub const MASK_WRITE_LIKE: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x0001_0000 | 0x0004_0000 | 0x0008_0000 | 0x4000_0000 | 0x1000_0000;

/// SDDL of `config.json`: like the root (not inheritable: it is a file) plus read + write data for `user`.
pub fn config_sddl(user: &str) -> Option<String> {
    is_sid_string(user).then(|| {
        format!("O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x{MASK_READ_EXECUTE:x};;;BU)(A;;0x{MASK_CONFIG_USER:x};;;{user})")
    })
}

/// true if `s` looks like a string SID (`S-1-<n>[-<n>...]`, non-empty decimal parts only): it is
/// spliced into SDDL, so nothing else may get through. Pure.
pub fn is_sid_string(s: &str) -> bool {
    s.len() <= 200
        && s.strip_prefix("S-1-")
            .is_some_and(|rest| rest.split('-').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
}

/// true if `sid` is SYSTEM or the Administrators group. Pure.
pub fn is_admin_sid(sid: &str) -> bool {
    sid.eq_ignore_ascii_case(SID_SYSTEM) || sid.eq_ignore_ascii_case(SID_ADMINISTRATORS)
}

/// One ACE of a DACL, reduced to what the checks need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AceFacts {
    /// Deny ACE (any deny type): it can only remove rights, so it never makes a folder unsafe.
    pub deny: bool,
    /// Trustee as string SID; `None` for ACE types whose layout is not parsed (treated as untrusted).
    pub sid: Option<String>,
    pub mask: u32,
}

/// true if the DACL grants no write-like right to anyone but SYSTEM/Administrators. A NULL DACL
/// (`None`) grants everything to everyone. Inherit-only ACEs count too (they reach the children). Pure.
pub fn dacl_admin_only(dacl: Option<&[AceFacts]>) -> bool {
    let Some(aces) = dacl else { return false };
    aces.iter().all(|a| a.deny || a.mask & MASK_WRITE_LIKE == 0 || a.sid.as_deref().is_some_and(is_admin_sid))
}

/// What was found at a data folder path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirFacts {
    pub is_dir: bool,
    pub is_reparse: bool,
    /// `Some(true)` = owned by SYSTEM/Administrators; `None` = owner unreadable.
    pub owner_trusted: Option<bool>,
    pub admin_only: bool,
}

/// Decision about an existing data folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Safe as is.
    Use,
    /// Admin-owned real folder with a DACL that lets non-admins write. The root is then moved aside
    /// and recreated (its children may have been planted); a subfolder of a verified root only gets
    /// the protected DACL re-applied.
    RepairDacl,
    /// Not owned by an admin (squatting): never used; may be renamed away.
    Hostile,
    /// Admin-owned but not usable (reparse point, not a folder, or a bad DACL we may not fix).
    Refuse,
}

/// Pure decision for a folder that already exists. `can_repair` = elevated (owner-admin can fix the DACL).
pub fn classify(f: &DirFacts, can_repair: bool) -> Verdict {
    if f.owner_trusted != Some(true) {
        return Verdict::Hostile;
    }
    if f.is_reparse || !f.is_dir {
        return Verdict::Refuse;
    }
    match (f.admin_only, can_repair) {
        (true, _) => Verdict::Use,
        (false, true) => Verdict::RepairDacl,
        (false, false) => Verdict::Refuse,
    }
}

/// Checks the attributes of an opened file: refuses reparse points, folders and (if `single_link`)
/// files with more than one hard link. Pure.
pub fn check_regular(attrs: u32, links: u32, single_link: bool) -> Result<(), &'static str> {
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        Err("it is a reparse point (symlink or junction)")
    } else if attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
        Err("it is a folder")
    } else if single_link && links != 1 {
        Err("it has more than one hard link")
    } else {
        Ok(())
    }
}

/// true if `final_path` (from `GetFinalPathNameByHandleW` on the opened file) is exactly
/// `<dir_final>\<name>` (case-insensitive). Pure.
pub fn final_path_matches(final_path: &str, dir_final: &str, name: &str) -> bool {
    let expected = format!("{}\\{}", dir_final.trim_end_matches('\\'), name);
    final_path.to_lowercase() == expected.to_lowercase()
}

/// Sends a line to the debugger output (DebugView): the fallback when there is no log file.
pub fn debug_out(msg: &str) {
    let s = HSTRING::from(format!("mini-oled-dock: {msg}\n"));
    // SAFETY: `s` is a NUL-terminated wide string that lives for the call.
    unsafe { OutputDebugStringW(&s) };
}

/// Who may change a file written by `write_atomic`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAcl {
    /// Inherit the folder's DACL (admin-only inside the root).
    Inherit,
    /// `config.json`: explicit protected DACL with the token user's read + write-data ACE.
    UserEditable,
}

/// Opens `path` for reading WITHOUT following reparse points and checks it is a regular file
/// (single hard link if `single_link`). Errors keep `NotFound` for a missing file.
pub fn open_checked(path: &Path, single_link: bool) -> io::Result<File> {
    open_checked_shared(path, single_link, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
}

/// `open_checked` with read sharing only: while the handle is open nobody can open the file for writing, truncate,
/// rename or delete it (a file mapped from it can never change under its reader, L3 T19).
pub fn open_checked_read_only(path: &Path, single_link: bool) -> io::Result<File> {
    open_checked_shared(path, single_link, FILE_SHARE_READ)
}

pub fn open_checked_shared(path: &Path, single_link: bool, share: FILE_SHARE_MODE) -> io::Result<File> {
    let f =
        OpenOptions::new().read(true).share_mode(share.0).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0).open(path)?;
    let info = file_info(&f)?;
    check_regular(info.dwFileAttributes, info.nNumberOfLinks, single_link)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e))?;
    Ok(f)
}

pub fn read_capped(f: File, cap: u64) -> io::Result<Vec<u8>> {
    let mut v = Vec::new();
    f.take(cap + 1).read_to_end(&mut v)?;
    if v.len() as u64 > cap {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("larger than {cap} bytes")));
    }
    Ok(v)
}

/// Reads a whole file (at most `cap` bytes) through a handle opened without following reparse
/// points; refuses reparse points, folders and (if `single_link`) hard-linked files.
pub fn read_file(path: &Path, cap: u64, single_link: bool) -> io::Result<Vec<u8>> {
    read_capped(open_checked(path, single_link)?, cap)
}

/// true for a staging folder name: 1..=32 chars of `[A-Za-z0-9.]`, not starting with `.`, no `..`.
/// Pure. Canonical release tags (`v1.4.0`) pass; separators, drive colons and spaces never do.
pub fn is_plain_dir_name(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && !s.starts_with('.')
        && !s.contains("..")
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
}

/// An existing folder that must classify `Use` as is (admin-owned, admin-only, not a reparse point);
/// returns its final path. Nothing is repaired here.
pub fn checked_dir(path: &Path) -> Result<String, String> {
    let shown = path.display();
    let dir = open_dir(path, false).map_err(|e| format!("cannot open {shown}: {e}"))?;
    let facts = dir_facts(&dir).map_err(|e| format!("cannot inspect {shown}: {e}"))?;
    match classify(&facts, false) {
        Verdict::Use => final_path(handle_of(&dir.handle)).ok_or_else(|| format!("{shown}: final path unavailable")),
        v => Err(format!("{shown} is not usable ({v:?})")),
    }
}

/// true for an existing folder no non-admin can write in: it classifies `Use` as is (a real folder, admin-owned, with an
/// admin-only DACL). `update::child_exe` asks it of the running exe's folder.
pub fn is_admin_only_dir(path: &Path) -> bool {
    checked_dir(path).is_ok()
}

/// Atomic write: `<path>.tmp` is created NEW (never through an existing name or link) with the
/// requested DACL, written, flushed, then renamed over `path` (a rename replaces the directory
/// entry, it never follows a link at the destination). Chosen over an in-place truncate+write
/// because the file is never seen half-written and the DACL travels with the new file.
pub fn write_atomic(path: &Path, bytes: &[u8], acl: FileAcl) -> Result<(), String> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    // A stale temp (crash): deleting a name removes the link itself, never a link target.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("removing {}: {e}", tmp.display())),
    }
    let sddl = match acl {
        FileAcl::Inherit => None,
        FileAcl::UserEditable => root().and_then(|r| r.user_sid.as_deref()).and_then(config_sddl),
    };
    let sd = sddl.as_deref().map(SecDesc::from_sddl).transpose()?;
    let sa = sd.as_ref().map(|sd| SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.as_ptr().0,
        bInheritHandle: BOOL(0),
    });
    let wide = HSTRING::from(tmp.as_os_str());
    // SAFETY: `wide` is NUL-terminated; `sa` (and the descriptor it points to, owned by `sd`) outlive
    // the call. CREATE_NEW + FILE_FLAG_OPEN_REPARSE_POINT never opens an existing name or link.
    let h = unsafe {
        CreateFileW(
            &wide,
            GENERIC_WRITE.0,
            windows::Win32::Storage::FileSystem::FILE_SHARE_MODE(0),
            sa.as_ref().map(|s| s as *const SECURITY_ATTRIBUTES),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|e| format!("creating {}: {e}", tmp.display()))?;
    // SAFETY: `h` is a valid file handle just returned by CreateFileW and owned by nobody else.
    let mut f = File::from(unsafe { OwnedHandle::from_raw_handle(h.0) });
    let res = f.write_all(bytes).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {}: {e}", tmp.display()));
    }
    let (src, dst) = (HSTRING::from(tmp.as_os_str()), HSTRING::from(path.as_os_str()));
    // SAFETY: both strings are NUL-terminated and live for the call.
    if let Err(e) = unsafe { MoveFileExW(&src, &dst, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("replacing {}: {e}", path.display()));
    }
    Ok(())
}

/// Opens `path` for appending (creating it if missing) without following reparse points; if the
/// name is a link or a hard-linked file it is deleted (the name only) and created fresh once.
pub fn open_append(path: &Path) -> Option<File> {
    for _ in 0..2 {
        let f = OpenOptions::new()
            .append(true)
            .create(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
            .ok()?;
        let ok = file_info(&f).is_ok_and(|i| check_regular(i.dwFileAttributes, i.nNumberOfLinks, true).is_ok());
        if ok {
            return Some(f);
        }
        drop(f);
        // Deleting a name never touches the target of a link.
        std::fs::remove_file(path).ok()?;
    }
    None
}

pub fn handle_of<T: AsRawHandle>(h: &T) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

pub fn file_info<T: AsRawHandle>(h: &T) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: valid open handle; `info` is a writable struct of the declared type.
    unsafe { GetFileInformationByHandle(handle_of(h), &mut info) }.map_err(io::Error::other)?;
    Ok(info)
}

/// Final normalized DOS path of an open handle (`\\?\C:\...`).
pub fn final_path(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 512];
    for _ in 0..2 {
        // SAFETY: `h` is a valid handle; `buf` is writable for its whole length.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                h,
                &mut buf,
                GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0),
            )
        } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return Some(String::from_utf16_lossy(&buf[..n]));
        }
        buf = vec![0u16; n + 1];
    }
    None
}

/// Opened folder (or whatever is at the path: reparse points are not followed).
pub struct OpenDir {
    pub handle: OwnedHandle,
    /// The handle has READ_CONTROL (owner/DACL readable).
    pub can_read_sd: bool,
}

pub fn open_raw(path: &Path, access: u32) -> windows::core::Result<OwnedHandle> {
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: `wide` is NUL-terminated and lives for the call; BACKUP_SEMANTICS opens folders and
    // OPEN_REPARSE_POINT opens a junction/symlink itself instead of its target.
    let h = unsafe {
        CreateFileW(
            &wide,
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }?;
    // SAFETY: fresh valid handle, ownership transferred to the OwnedHandle.
    Ok(unsafe { OwnedHandle::from_raw_handle(h.0) })
}

/// Opens a data folder. `for_write` (elevated): tries to also get WRITE_DAC + DELETE (repair,
/// rename away), falling back to read-only access, and finally to DELETE alone (owner unreadable).
pub fn open_dir(path: &Path, for_write: bool) -> Result<OpenDir, String> {
    let ra = FILE_READ_ATTRIBUTES.0;
    let rc = READ_CONTROL.0;
    let attempts: &[u32] = if for_write {
        &[rc | WRITE_DAC.0 | DELETE.0 | ra, rc | WRITE_DAC.0 | ra, rc | ra, DELETE.0 | ra]
    } else {
        &[rc | ra]
    };
    let mut last = String::new();
    for &access in attempts {
        match open_raw(path, access) {
            Ok(handle) => return Ok(OpenDir { handle, can_read_sd: access & rc != 0 }),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

pub fn dir_facts(d: &OpenDir) -> Result<DirFacts, String> {
    let info = file_info(&d.handle).map_err(|e| e.to_string())?;
    let attrs = info.dwFileAttributes;
    let (owner_trusted, admin_only) = if d.can_read_sd {
        let sec = read_security(handle_of(&d.handle))?;
        (Some(sec.owner.as_deref().is_some_and(is_admin_sid)), dacl_admin_only(sec.dacl.as_deref()))
    } else {
        (None, false)
    };
    Ok(DirFacts {
        is_dir: attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
        is_reparse: attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0,
        owner_trusted,
        admin_only,
    })
}

pub struct SecInfo {
    pub owner: Option<String>,
    /// `None` = NULL DACL.
    pub dacl: Option<Vec<AceFacts>>,
}

/// Owner and DACL of an open handle (needs READ_CONTROL).
pub fn read_security(h: HANDLE) -> Result<SecInfo, String> {
    let what = (OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION).0;
    let mut needed = 0u32;
    // SAFETY: size query with no buffer; `needed` is writable. The expected "buffer too small" error is ignored.
    let _ = unsafe { GetKernelObjectSecurity(h, what, None, 0, &mut needed) };
    if needed == 0 {
        return Err("GetKernelObjectSecurity: no size".into());
    }
    // u64 storage: the self-relative descriptor needs pointer alignment.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let sd = PSECURITY_DESCRIPTOR(buf.as_mut_ptr().cast());
    // SAFETY: `buf` holds at least `needed` writable bytes; `h` is a valid handle.
    unsafe { GetKernelObjectSecurity(h, what, Some(sd), needed, &mut needed) }
        .map_err(|e| format!("GetKernelObjectSecurity: {e}"))?;
    let mut owner = PSID::default();
    let mut defaulted = BOOL(0);
    // SAFETY: `sd` is the valid self-relative descriptor in `buf`; out-params are writable. The returned
    // SID points into `buf`, which outlives its use below.
    unsafe { GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted) }.map_err(|e| format!("owner: {e}"))?;
    let owner = sid_string(owner);
    let mut present = BOOL(0);
    let mut acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: as above; the ACL pointer points into `buf`.
    unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted) }
        .map_err(|e| format!("dacl: {e}"))?;
    let dacl = if present.as_bool() && !acl.is_null() { Some(read_aces(acl)?) } else { None };
    drop(buf);
    Ok(SecInfo { owner, dacl })
}

/// ACE type codes (winnt.h) of the deny family: plain, object, callback, callback-object.
pub const DENY_ACE_TYPES: [u8; 4] = [1, 6, 10, 12];

pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

fn read_aces(acl: *const ACL) -> Result<Vec<AceFacts>, String> {
    let mut info = ACL_SIZE_INFORMATION::default();
    // SAFETY: `acl` is a valid ACL (from a valid descriptor); `info` is writable and sized as declared.
    unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast::<c_void>(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|e| format!("GetAclInformation: {e}"))?;
    let mut out = Vec::with_capacity(info.AceCount as usize);
    for i in 0..info.AceCount {
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: `i` < AceCount; `p` is writable.
        unsafe { GetAce(acl, i, &mut p) }.map_err(|e| format!("GetAce: {e}"))?;
        if p.is_null() {
            return Err("GetAce: null".into());
        }
        // SAFETY: every ACE starts with ACE_HEADER followed by the 32-bit access mask (all ACE types);
        // unaligned reads avoid any alignment assumption.
        let (hdr, mask) = unsafe {
            (
                std::ptr::read_unaligned(p.cast::<ACE_HEADER>()),
                std::ptr::read_unaligned(p.cast::<u8>().add(4).cast::<u32>()),
            )
        };
        let deny = DENY_ACE_TYPES.contains(&hdr.AceType);
        let sid = if hdr.AceType == ACCESS_ALLOWED_ACE_TYPE || hdr.AceType == DENY_ACE_TYPES[0] {
            // SAFETY: ACCESS_ALLOWED_ACE / ACCESS_DENIED_ACE: header (4) + mask (4) + SID.
            sid_string(PSID(unsafe { p.cast::<u8>().add(8) }.cast()))
        } else {
            None
        };
        out.push(AceFacts { deny, sid, mask });
    }
    Ok(out)
}

/// String form of a SID (`S-1-...`).
fn sid_string(sid: PSID) -> Option<String> {
    if sid.0.is_null() {
        return None;
    }
    let mut s = PWSTR::null();
    // SAFETY: `sid` points to a valid SID; `s` receives a LocalAlloc'd string freed below.
    unsafe { ConvertSidToStringSidW(sid, &mut s) }.ok()?;
    // SAFETY: `s` is a valid NUL-terminated string returned by the call above.
    let out = unsafe { s.to_string() }.ok();
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc; not used afterwards.
    unsafe { LocalFree(Some(HLOCAL(s.0.cast()))) };
    out
}

/// Token user of this process as string SID (the account whose desktop the dock serves).
pub fn token_user_sid() -> Option<String> {
    let mut token = HANDLE::default();
    // SAFETY: pseudo-handle of the current process; `token` is writable.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    // SAFETY: fresh valid token handle, closed when `_owned` drops.
    let _owned = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let mut len = 0u32;
    // SAFETY: size query; `len` is writable. The expected "buffer too small" error is ignored.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    if (len as usize) < std::mem::size_of::<TOKEN_USER>() {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` writable, pointer-aligned bytes.
    unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len) }.ok()?;
    // SAFETY: the buffer now starts with a TOKEN_USER whose SID points inside `buf` (alive here).
    let tu = unsafe { std::ptr::read(buf.as_ptr().cast::<TOKEN_USER>()) };
    sid_string(tu.User.Sid)
}

pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: `id` is a valid KNOWNFOLDERID; no token = current user. The PWSTR is freed below.
    let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: `p` is a NUL-terminated string returned by the call above.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: allocated with CoTaskMemAlloc by SHGetKnownFolderPath; not used afterwards.
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s.filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// Self-relative security descriptor built from SDDL (LocalFree'd on drop).
pub struct SecDesc(PSECURITY_DESCRIPTOR);

impl SecDesc {
    pub fn from_sddl(sddl: &str) -> Result<SecDesc, String> {
        let wide = HSTRING::from(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor owned by the result.
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&wide, SDDL_REVISION_1, &mut sd, None) }
            .map_err(|e| format!("SDDL: {e}"))?;
        Ok(SecDesc(sd))
    }

    /// The raw descriptor, valid while `self` lives.
    pub fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }
}

impl Drop for SecDesc {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW; freed once.
        unsafe { LocalFree(Some(HLOCAL(self.0 .0))) };
    }
}

// ------------------------------------------------------------------ the registered root

/// The data root a process works with, as verified by whoever registered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootInfo {
    pub path: PathBuf,
    /// Elevated and verified: the process may write in it.
    pub writable: bool,
    /// Token user as string SID (the ACE of `config.json`).
    pub user_sid: Option<String>,
}

static ROOT: OnceLock<Option<RootInfo>> = OnceLock::new();

/// Registers the root of this process (`None` = no usable root). The first call wins; `false` if a root was
/// already registered.
pub fn register(root: Option<RootInfo>) -> bool {
    ROOT.set(root).is_ok()
}

/// The registered root; `None` before `register` and when there is no usable root.
pub fn root() -> Option<&'static RootInfo> {
    ROOT.get().and_then(Option::as_ref)
}

/// true if this process may write in the registered root.
pub fn writable() -> bool {
    root().is_some_and(|r| r.writable)
}

/// `%ProgramData%\mini-oled-dock`. Never creates anything and never registers.
pub fn lookup_root() -> Option<PathBuf> {
    known_folder(&FOLDERID_ProgramData).map(|p| p.join(ROOT_NAME))
}

/// `<root>\suite` of the registered root, if it exists and is admin-only (it is created by the elevated dock).
pub fn suite_dir() -> Option<PathBuf> {
    admin_subdir("suite")
}

/// `<root>\journal` of the registered root, same rule as `suite_dir`.
pub fn journal_dir() -> Option<PathBuf> {
    admin_subdir("journal")
}

fn admin_subdir(name: &str) -> Option<PathBuf> {
    let dir = root()?.path.join(name);
    checked_dir(&dir).ok().map(|_| dir)
}

/// For apps and helpers: the root as it is on disk, accepted only if it already classifies `Verdict::Use`
/// (a real folder, admin-owned, admin-only DACL). `writable` is `elevated`. Registers what it returns;
/// never creates or repairs.
pub fn open_existing(elevated: bool) -> Option<RootInfo> {
    let info = open_existing_at(&lookup_root()?, elevated)?;
    register(Some(info.clone()));
    Some(info)
}

fn open_existing_at(path: &Path, elevated: bool) -> Option<RootInfo> {
    let facts = open_dir(path, false).and_then(|d| dir_facts(&d)).ok()?;
    (classify(&facts, false) == Verdict::Use).then(|| RootInfo {
        path: path.to_path_buf(),
        writable: elevated,
        user_sid: token_user_sid(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION};

    fn allow(sid: &str, mask: u32) -> AceFacts {
        AceFacts { deny: false, sid: Some(sid.into()), mask }
    }

    fn elevated() -> bool {
        let mut token = HANDLE::default();
        // SAFETY: pseudo-handle of the current process; `token` is writable.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
            return false;
        }
        // SAFETY: fresh valid token handle, closed when `_owned` drops.
        let _owned = unsafe { OwnedHandle::from_raw_handle(token.0) };
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        // SAFETY: `e` is a TOKEN_ELEVATION of the size passed; `len` is writable.
        let r = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut e as *mut TOKEN_ELEVATION).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
        };
        r.is_ok() && e.TokenIsElevated != 0
    }

    /// Creates `path` with the descriptor of `sddl` (the dock's `create_dir` does the same in its own module).
    fn make_dir(path: &Path, sddl: &str) -> bool {
        let Ok(sd) = SecDesc::from_sddl(sddl) else { return false };
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.as_ptr().0,
            bInheritHandle: BOOL(0),
        };
        let wide = HSTRING::from(path.as_os_str());
        // SAFETY: `wide` is NUL-terminated; `sa` and the descriptor it points to (owned by `sd`) outlive the call.
        unsafe { windows::Win32::Storage::FileSystem::CreateDirectoryW(&wide, Some(&sa)) }.is_ok()
    }

    fn temp_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("oled_sys_datadir_{tag}_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&base);
        base
    }

    #[test]
    fn masks_match_the_documented_values() {
        assert_eq!(MASK_READ_EXECUTE, 0x1200a9);
        assert_eq!(MASK_CONFIG_USER, 0x12019f);
        // The config ACE must not allow delete, WRITE_DAC or WRITE_OWNER.
        assert_eq!(MASK_CONFIG_USER & (DELETE.0 | WRITE_DAC.0 | 0x0008_0000), 0);
        assert_eq!(MASK_READ_EXECUTE & MASK_WRITE_LIKE, 0);
    }

    #[test]
    fn sddl_strings() {
        let sid = "S-1-5-21-1-2-3-1001";
        assert_eq!(
            config_sddl(sid).as_deref(),
            Some("O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;BU)(A;;0x12019f;;;S-1-5-21-1-2-3-1001)")
        );
        // SDDL injection through the SID is impossible.
        assert!(config_sddl("S-1-5)(A;;FA;;;WD").is_none());
        assert!(config_sddl("").is_none());
    }

    #[test]
    fn sddl_strings_parse() {
        let s = config_sddl("S-1-5-21-1-2-3-1001").unwrap_or_default();
        assert!(SecDesc::from_sddl(&s).is_ok(), "{s}");
    }

    #[test]
    fn sid_strings() {
        assert!(is_sid_string("S-1-5-18"));
        assert!(!is_sid_string("S-1-"));
        assert!(is_admin_sid("S-1-5-32-544") && is_admin_sid("s-1-5-18"));
        assert!(!is_admin_sid("S-1-5-32-545") && !is_admin_sid("S-1-5-21-1-2-3-1001"));
    }

    #[test]
    fn dacl_checks() {
        let ours = [allow(SID_SYSTEM, 0x1f01ff), allow(SID_ADMINISTRATORS, 0x1f01ff), allow("S-1-5-32-545", 0x1200a9)];
        assert!(dacl_admin_only(Some(&ours)));
        // NULL DACL = full access for everyone.
        assert!(!dacl_admin_only(None));
        // ProgramData's default: Users may create folders / append (0x4) and write data (0x2).
        assert!(!dacl_admin_only(Some(&[allow("S-1-5-32-545", 0x1200a9 | 0x6)])));
        // CREATOR OWNER full control (inherit-only) is still a writer.
        assert!(!dacl_admin_only(Some(&[allow("S-1-3-0", 0x1000_0000)])));
        for m in [0x2, 0x4, 0x10, 0x40, 0x100, 0x1_0000, 0x4_0000, 0x8_0000, 0x4000_0000, 0x1000_0000] {
            assert!(!dacl_admin_only(Some(&[allow("S-1-1-0", m)])), "{m:#x}");
        }
        // Deny ACEs never weaken; unknown ACE types with write rights do.
        assert!(dacl_admin_only(Some(&[AceFacts { deny: true, sid: None, mask: 0x1f01ff }])));
        assert!(!dacl_admin_only(Some(&[AceFacts { deny: false, sid: None, mask: 0x2 }])));
        assert!(dacl_admin_only(Some(&[AceFacts { deny: false, sid: None, mask: 0x1200a9 }])));
    }

    #[test]
    fn classify_decisions() {
        let good = DirFacts { is_dir: true, is_reparse: false, owner_trusted: Some(true), admin_only: true };
        assert_eq!(classify(&good, true), Verdict::Use);
        assert_eq!(classify(&good, false), Verdict::Use);
        let loose = DirFacts { admin_only: false, ..good };
        assert_eq!(classify(&loose, true), Verdict::RepairDacl);
        assert_eq!(classify(&loose, false), Verdict::Refuse);
        // Squatting: owned by a user, or owner unreadable, whatever else it is.
        for owner in [Some(false), None] {
            assert_eq!(classify(&DirFacts { owner_trusted: owner, ..good }, true), Verdict::Hostile);
            let junction = DirFacts { owner_trusted: owner, is_reparse: true, ..good };
            assert_eq!(classify(&junction, true), Verdict::Hostile);
        }
        // Admin-owned junction or file: refused, never repaired.
        assert_eq!(classify(&DirFacts { is_reparse: true, ..good }, true), Verdict::Refuse);
        assert_eq!(classify(&DirFacts { is_dir: false, ..good }, true), Verdict::Refuse);
    }

    #[test]
    fn regular_file_checks() {
        let normal = FILE_ATTRIBUTE_NORMAL.0;
        assert!(check_regular(normal, 1, true).is_ok());
        assert!(check_regular(normal, 2, false).is_ok());
        assert!(check_regular(normal, 2, true).is_err());
        assert!(check_regular(normal | FILE_ATTRIBUTE_REPARSE_POINT.0, 1, true).is_err());
        assert!(check_regular(FILE_ATTRIBUTE_DIRECTORY.0, 1, false).is_err());
    }

    #[test]
    fn final_path_checks() {
        let dir = r"\\?\C:\ProgramData\mini-oled-dock\icons";
        assert!(final_path_matches(r"\\?\C:\ProgramData\mini-oled-dock\icons\a.png", dir, "a.png"));
        assert!(final_path_matches(r"\\?\c:\programdata\MINI-OLED-DOCK\Icons\A.PNG", dir, "a.png"));
        assert!(final_path_matches(r"\\?\C:\ProgramData\mini-oled-dock\icons\a.png", &format!("{dir}\\"), "a.png"));
        // Redirected (hard link elsewhere, symlinked parent, short-name trick): refused.
        assert!(!final_path_matches(r"\\?\C:\Windows\System32\a.png", dir, "a.png"));
        assert!(!final_path_matches(r"\\?\C:\ProgramData\mini-oled-dock\icons\sub\a.png", dir, "a.png"));
        assert!(!final_path_matches(r"\\?\C:\ProgramData\mini-oled-dock\icons\b.png", dir, "a.png"));
    }

    #[test]
    fn plain_dir_names() {
        for ok in ["v1.4.0", "v10.0.12"] {
            assert!(is_plain_dir_name(ok), "{ok}");
        }
        let long = "a".repeat(33);
        for bad in ["", ".", "..", ".v1", "v1..4", "v1/4", r"v1\4", "v1.4.0 ", "C:", "v1:4", long.as_str()] {
            assert!(!is_plain_dir_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn atomic_write_and_checked_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("dock_datadir_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("f.txt");
        assert!(write_atomic(&p, b"one", FileAcl::Inherit).is_ok());
        assert!(write_atomic(&p, b"two", FileAcl::Inherit).is_ok());
        assert_eq!(read_file(&p, 16, true).ok().as_deref(), Some(&b"two"[..]));
        assert!(read_file(&p, 2, true).is_err());
        assert_eq!(
            read_file(&dir.join("missing"), 16, true).map_err(|e| e.kind()).err(),
            Some(io::ErrorKind::NotFound)
        );
        // A folder is not a regular file.
        assert!(read_file(&dir, 16, false).is_err());
        // A hard-linked file is refused when a single link is required, and `open_append` replaces
        // the linked name with a fresh file instead of appending through it.
        let link = dir.join("link.txt");
        assert!(std::fs::hard_link(&p, &link).is_ok());
        assert!(read_file(&p, 16, true).is_err());
        assert!(read_file(&p, 16, false).is_ok());
        let appended = open_append(&link).map(|mut f| f.write_all(b"x").is_ok());
        assert_eq!(appended, Some(true));
        assert_eq!(read_file(&p, 16, true).ok().as_deref(), Some(&b"two"[..]));
        assert_eq!(read_file(&link, 16, true).ok().as_deref(), Some(&b"x"[..]));
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn register_is_once() {
        let info =
            RootInfo { path: std::env::temp_dir().join("oled_sys_register_probe"), writable: false, user_sid: None };
        assert!(register(Some(info.clone())));
        assert!(!register(None), "the first call wins");
        assert_eq!(root(), Some(&info));
        assert!(!writable());
        // The registered root has no suite or journal folder: both stay None (they are never created here).
        assert!(suite_dir().is_none() && journal_dir().is_none());
    }

    #[test]
    fn lookup_root_never_creates() {
        let Some(p) = lookup_root() else { return };
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some(ROOT_NAME));
        let before = p.exists();
        let _ = lookup_root();
        assert_eq!(p.exists(), before, "looking up the root must not create it");
    }

    #[test]
    fn open_existing_refuses_a_loose_dacl() {
        let base = temp_base("loose");
        let d = base.join("root");
        // Everyone may write: never a usable root, whoever owns it.
        assert!(make_dir(&d, "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;WD)"));
        assert_eq!(open_existing_at(&d, true), None);
        assert_eq!(open_existing_at(&base.join("missing"), true), None);
        let _ = std::fs::remove_dir(&d);
        let _ = std::fs::remove_dir(&base);
    }

    #[test]
    fn open_existing_accepts_an_admin_only_folder() {
        if !elevated() {
            println!("skipped: the test runner is not elevated (a folder it creates is not admin-owned)");
            return;
        }
        let base = temp_base("admin");
        let d = base.join("root");
        assert!(make_dir(&d, "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"));
        let info = open_existing_at(&d, true).expect("an admin-only folder is accepted");
        assert!(info.writable && info.path == d);
        assert!(!open_existing_at(&d, false).is_some_and(|i| i.writable));
        let _ = std::fs::remove_dir(&d);
        let _ = std::fs::remove_dir(&base);
    }
}
