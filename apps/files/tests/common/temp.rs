//! TempRoot: the only place where tests create, change and delete files (plan rule D2). A root is a fresh folder
//! %TEMP%\mod-files-test-<pid>-<n>; every helper takes a path relative to it ("a/b.txt" or "a\b.txt", no ".", "..",
//! drive or stream colon). Drop removes the tree after checking the prefix again, walking it with FindFirstFileExW
//! without ever entering a reparse point (a junction or a symbolic link is removed as a link, its target is never
//! read or written) and clearing read-only attributes first; a root whose path no longer passes the check panics
//! without deleting anything.
//!
//! The probes' LiveRoot (examples/common/live.rs, %TEMP%\mod-files-live-*) includes this file and reuses `fence`, so
//! both fences share one implementation. Included three ways (the library's unit tests, tests/common and the probes),
//! so not every item is used by every includer.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// The folder-name prefix of every TempRoot.
pub const TEST_PREFIX: &str = "mod-files-test-";

/// FNV-1a 64: enough for equality of test files.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A test's own folder under %TEMP%, removed with everything in it when dropped.
pub struct TempRoot {
    path: PathBuf,
    tag: Box<str>,
    denied: Mutex<Vec<PathBuf>>,
}

impl TempRoot {
    /// A new, empty %TEMP%\mod-files-test-<pid>-<n>. Panics when %TEMP% or one of its parents is a reparse point.
    pub fn new(tag: &str) -> TempRoot {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let base = fence::temp_base();
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{TEST_PREFIX}{}-{n}", std::process::id()));
            // create_dir, not create_dir_all: a folder left by a dead process with the same id is skipped, never reused.
            match std::fs::create_dir(&path) {
                Ok(()) => return TempRoot { path, tag: tag.into(), denied: Mutex::new(Vec::new()) },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1_000_000 => continue,
                Err(e) => panic!("TempRoot {tag}: cannot create {}: {e}", path.display()),
            }
        }
    }

    /// A root over an arbitrary path, for the fence's own test only: its Drop must refuse and delete nothing. It
    /// comes as a ForgedRoot, which has none of TempRoot's methods, so it can never create, change or delete a file.
    #[doc(hidden)]
    pub fn forged(path: PathBuf) -> ForgedRoot {
        ForgedRoot(TempRoot { path, tag: "forged".into(), denied: Mutex::new(Vec::new()) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The full path of `rel` (nothing is created).
    pub fn at(&self, rel: &str) -> PathBuf {
        fence::resolve(&self.path, rel)
    }

    /// Creates the file `rel` (and its parent folders) with `bytes`.
    pub fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        fence::file(&self.path, rel, bytes)
    }

    /// Creates a file named by raw UTF-16 units directly in the root (names that are not valid UTF-16).
    pub fn file_wide(&self, name_units: &[u16], bytes: &[u8]) -> PathBuf {
        fence::file_wide(&self.path, name_units, bytes)
    }

    /// Creates the folder `rel` and its parents.
    pub fn dir(&self, rel: &str) -> PathBuf {
        fence::dir(&self.path, rel)
    }

    /// Creates the folder `rel` and its parents by their exact names: a trailing dot or space is kept.
    pub fn dir_exact(&self, rel: &str) -> PathBuf {
        fence::dir_exact(&self.path, rel)
    }

    /// Overwrites the existing file `rel`.
    pub fn write(&self, rel: &str, bytes: &[u8]) {
        fence::write(&self.path, rel, bytes)
    }

    /// Removes the file `rel`.
    pub fn remove(&self, rel: &str) {
        fence::remove(&self.path, rel)
    }

    /// Removes the empty folder `rel`.
    pub fn remove_dir(&self, rel: &str) {
        fence::remove_dir(&self.path, rel)
    }

    pub fn rename(&self, from: &str, to: &str) {
        fence::rename(&self.path, from, to)
    }

    /// Renames `from` to `to` and gives the result back: a rename that a held handle must refuse.
    pub fn try_rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        fence::try_rename(&self.path, from, to)
    }

    /// Removes the empty folder `rel` and gives the result back.
    pub fn try_remove_dir(&self, rel: &str) -> std::io::Result<()> {
        fence::try_remove_dir(&self.path, rel)
    }

    /// Sets the last-write time of `rel` (a FILETIME value).
    pub fn touch(&self, rel: &str, mtime: i64) {
        fence::touch(&self.path, rel, mtime)
    }

    pub fn set_readonly(&self, rel: &str, on: bool) {
        fence::set_readonly(&self.path, rel, on)
    }

    /// Writes the alternate data stream `rel:stream`.
    pub fn ads(&self, rel: &str, stream: &str, bytes: &[u8]) {
        fence::ads(&self.path, rel, stream, bytes)
    }

    /// Makes `rel` a junction (mount point) to `target`, which must be a folder inside a TempRoot.
    pub fn junction(&self, rel: &str, target: &Path) {
        fence::junction(&self.path, rel, target, TEST_PREFIX)
    }

    /// FNV-1a 64 of the bytes of the file `rel`.
    pub fn hash(&self, rel: &str) -> u64 {
        fence::hash(&self.path, rel)
    }

    /// Every file and stream under `rel` with its hash, every folder, and every junction with its target; a
    /// reparse point is recorded, never followed.
    pub fn hash_tree(&self, rel: &str) -> BTreeMap<String, String> {
        fence::hash_tree(&self.path, rel)
    }

    /// Denies this user the listing of the folder `rel` (an explicit deny ACE); Drop restores it.
    pub fn deny_listing(&self, rel: &str) {
        let p = fence::resolve(&self.path, rel);
        fence::deny_listing_at(&p);
        if let Ok(mut d) = self.denied.lock() {
            d.push(p);
        }
    }

    /// Removes the deny ACE of `deny_listing`.
    pub fn allow_listing(&self, rel: &str) {
        let p = fence::resolve(&self.path, rel);
        fence::allow_listing_at(&p);
        if let Ok(mut d) = self.denied.lock() {
            d.retain(|x| x != &p);
        }
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        if !fence::is_fenced(&self.path, TEST_PREFIX) {
            let msg = format!(
                "TempRoot {}: refusing to delete {}: not a {TEST_PREFIX} folder directly under %TEMP%",
                self.tag,
                self.path.display()
            );
            if std::thread::panicking() {
                eprintln!("{msg}");
                return;
            }
            panic!("{msg}");
        }
        let denied = self.denied.lock().map(|d| d.clone()).unwrap_or_default();
        for d in denied {
            fence::allow_listing_at(&d);
        }
        let errors = fence::remove_tree(&self.path);
        if !errors.is_empty() {
            eprintln!("TempRoot {}: left behind: {errors:?}", self.tag);
        }
    }
}

/// What `TempRoot::forged` returns: a TempRoot over any path, behind a type with no methods. A test can only drop it,
/// through TempRoot's own Drop, which refuses a path outside the fence and deletes nothing.
#[doc(hidden)]
pub struct ForgedRoot(TempRoot);

/// Writes a test record under the cargo target directory, at `rel` ("natural/..." or "disposable/..." only).
pub fn artifact(rel: &str, bytes: &[u8]) -> PathBuf {
    assert!(
        rel.starts_with("natural/") || rel.starts_with("disposable/"),
        "artifact: {rel:?} must start with natural/ or disposable/"
    );
    assert!(!rel.contains(['\\', ':']), "artifact: {rel:?} uses '/' separators and no colon");
    assert!(rel.split('/').all(|c| !c.is_empty() && c != "." && c != ".."), "artifact: {rel:?} has a bad component");
    let path = rel.split('/').fold(target_dir(), |p, c| p.join(c));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("artifact: {}: {e}", parent.display()));
    }
    std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("artifact: {}: {e}", path.display()));
    path
}

/// The cargo target directory: CARGO_TARGET_DIR when it is absolute, else three levels above the test executable
/// (<target>\<profile>\deps\<test>.exe).
pub fn target_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).filter(|d| d.is_absolute()) {
        return d;
    }
    let exe = std::env::current_exe().expect("the test executable's path");
    exe.parent().and_then(Path::parent).and_then(Path::parent).expect("<target>\\<profile>\\deps\\<exe>").to_path_buf()
}

/// The shared implementation of TempRoot and LiveRoot: every call that changes a file is here.
pub mod fence {
    use std::collections::BTreeMap;
    use std::ffi::{c_void, OsString};
    use std::fs::File;
    use std::io::Write;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        LocalFree, ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_NO_MORE_FILES, FILETIME, GENERIC_WRITE, HANDLE, HLOCAL,
    };
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, UNPROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, DeleteFileW, FindClose, FindExInfoBasic, FindExSearchNameMatch,
        FindFirstFileExW, FindFirstStreamW, FindNextFileW, FindNextStreamW, FindStreamInfoStandard, GetFileAttributesW,
        RemoveDirectoryW, SetFileAttributesW, SetFileTime, CREATE_ALWAYS, CREATE_NEW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FIND_FIRST_EX_FLAGS, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING,
        WIN32_FIND_DATAW, WIN32_FIND_STREAM_DATA,
    };
    use windows::Win32::System::Ioctl::{FSCTL_GET_REPARSE_POINT, FSCTL_SET_REPARSE_POINT};
    use windows::Win32::System::SystemServices::IO_REPARSE_TAG_MOUNT_POINT;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::System::IO::DeviceIoControl;

    use super::fnv1a64;

    /// One entry of a folder listing.
    pub struct Found {
        pub name: Vec<u16>,
        pub attrs: u32,
        pub reparse_tag: u32,
    }

    /// %TEMP%, checked: absolute and no reparse point in its chain; in its long form.
    pub fn temp_base() -> PathBuf {
        let base = std::env::temp_dir();
        assert!(base.is_absolute(), "%TEMP% is not absolute: {}", base.display());
        if let Err(e) = check_chain(&base) {
            panic!("refusing a test root under {}: {e}", base.display());
        }
        temp_long()
    }

    /// %TEMP% with its 8.3 components expanded (C:\Users\RUNNER~1 on the CI runner): the shell reports long paths, so
    /// the test roots and the paths the tests expect are long too. %TEMP% itself when it cannot be read.
    pub fn temp_long() -> PathBuf {
        let base = std::env::temp_dir();
        let long = std::fs::canonicalize(&base).ok().and_then(|p| {
            let s = p.to_str()?.to_string();
            s.strip_prefix(r"\\?\").filter(|rest| !rest.starts_with("UNC\\")).map(PathBuf::from)
        });
        long.unwrap_or(base)
    }

    /// No existing component of `p` (the path itself and every parent) is a reparse point.
    pub fn check_chain(p: &Path) -> Result<(), String> {
        for a in p.ancestors() {
            if let Ok(m) = std::fs::symlink_metadata(a) {
                if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                    return Err(format!("{} is a reparse point", a.display()));
                }
            }
        }
        Ok(())
    }

    fn lower(p: &Path) -> String {
        p.to_string_lossy().trim_end_matches('\\').to_lowercase()
    }

    /// `path` is a folder directly under %TEMP% whose name starts with `prefix`, with no reparse point in its chain.
    pub fn is_fenced(path: &Path, prefix: &str) -> bool {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            return false;
        };
        (lower(parent) == lower(&std::env::temp_dir()) || lower(parent) == lower(&temp_long()))
            && name.starts_with(prefix)
            && name.len() > prefix.len()
            && !name.contains(['\\', '/', ':'])
            && check_chain(path).is_ok()
    }

    /// `path` is a fenced folder or lies inside one (no "." or ".." component, no reparse point in its chain).
    pub fn within_fenced(path: &Path, prefix: &str) -> bool {
        let plain = path.components().all(|c| {
            matches!(
                c,
                std::path::Component::Prefix(_) | std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        });
        plain && path.ancestors().any(|a| is_fenced(a, prefix)) && check_chain(path).is_ok()
    }

    /// `root` joined with the relative path `rel` ("" is the root itself). Panics on an absolute path, a colon or a
    /// "." or ".." component.
    pub fn resolve(root: &Path, rel: &str) -> PathBuf {
        assert!(!rel.contains(':'), "test path {rel:?}: no drive or stream colon");
        assert!(!rel.starts_with(['\\', '/']), "test path {rel:?} must be relative");
        let mut p = root.to_path_buf();
        for c in rel.split(['\\', '/']).filter(|c| !c.is_empty()) {
            assert!(c != "." && c != "..", "test path {rel:?}: no . or .. component");
            p.push(c);
        }
        p
    }

    /// The \\?\ form of an absolute path, NUL-terminated (long paths, no normalisation by Win32).
    pub fn verbatim(p: &Path) -> Vec<u16> {
        let s: Vec<u16> =
            p.as_os_str().encode_wide().map(|u| if u == u16::from(b'/') { u16::from(b'\\') } else { u }).collect();
        let w = |t: &str| t.encode_utf16().collect::<Vec<u16>>();
        let mut out = Vec::with_capacity(s.len() + 9);
        if s.starts_with(&w(r"\\?\")) {
            out.extend_from_slice(&s);
        } else if s.starts_with(&w(r"\\")) {
            out.extend(w(r"\\?\UNC\"));
            out.extend_from_slice(&s[2..]);
        } else {
            out.extend(w(r"\\?\"));
            out.extend_from_slice(&s);
        }
        out.push(0);
        out
    }

    fn with_suffix(p: &Path, suffix: &[u16]) -> Vec<u16> {
        let mut w = verbatim(p);
        w.pop();
        w.extend_from_slice(suffix);
        w.push(0);
        w
    }

    fn not_a_link(p: &Path) {
        if let Ok(m) = std::fs::symlink_metadata(p) {
            assert!(m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0, "{} is a reparse point", p.display());
        }
    }

    /// Opens `w` (a verbatim path) without following a reparse point.
    fn open_raw(w: &[u16], access: u32) -> windows::core::Result<OwnedHandle> {
        // SAFETY: `w` is NUL-terminated and lives for the call.
        let h = unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }?;
        // SAFETY: a fresh valid handle; the OwnedHandle closes it.
        Ok(unsafe { OwnedHandle::from_raw_handle(h.0) })
    }

    /// Creates a new file at `w` (a verbatim path, a stream allowed) and writes `bytes`.
    fn create_raw(
        w: &[u16],
        disposition: windows::Win32::Storage::FileSystem::FILE_CREATION_DISPOSITION,
        bytes: &[u8],
    ) {
        // SAFETY: `w` is NUL-terminated and lives for the call.
        let h = unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                disposition,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .unwrap_or_else(|e| panic!("create {}: {e}", String::from_utf16_lossy(w)));
        // SAFETY: a fresh valid file handle; the File owns and closes it.
        let mut f = File::from(unsafe { OwnedHandle::from_raw_handle(h.0) });
        f.write_all(bytes).unwrap_or_else(|e| panic!("write {}: {e}", String::from_utf16_lossy(w)));
    }

    pub fn file(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = resolve(root, rel);
        not_a_link(&p);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
        }
        std::fs::write(&p, bytes).unwrap_or_else(|e| panic!("write {}: {e}", p.display()));
        p
    }

    pub fn file_wide(root: &Path, name: &[u16], bytes: &[u8]) -> PathBuf {
        let bad = [0, u16::from(b'\\'), u16::from(b'/'), u16::from(b':')];
        assert!(
            !name.is_empty() && !name.iter().any(|u| bad.contains(u)),
            "file_wide: one name, no separator or colon"
        );
        let mut suffix = vec![u16::from(b'\\')];
        suffix.extend_from_slice(name);
        create_raw(&with_suffix(root, &suffix), CREATE_NEW, bytes);
        root.join(OsString::from_wide(name))
    }

    pub fn dir(root: &Path, rel: &str) -> PathBuf {
        let p = resolve(root, rel);
        std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("create {}: {e}", p.display()));
        p
    }

    /// Creates the folder `rel` and its parents by their exact names: a trailing dot or space is kept (the \\?\ form).
    pub fn dir_exact(root: &Path, rel: &str) -> PathBuf {
        let mut p = root.to_path_buf();
        for c in resolve(root, rel).strip_prefix(root).iter().flat_map(|r| r.components()) {
            p.push(c);
            let w = verbatim(&p);
            // SAFETY: `w` is NUL-terminated and lives for the call.
            let _ = unsafe { CreateDirectoryW(PCWSTR(w.as_ptr()), None) };
        }
        assert!(is_dir_exact(&p), "create {}", p.display());
        p
    }

    pub fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let p = resolve(root, rel);
        not_a_link(&p);
        assert!(p.is_file(), "write: {} is not an existing file", p.display());
        std::fs::write(&p, bytes).unwrap_or_else(|e| panic!("write {}: {e}", p.display()));
    }

    pub fn remove(root: &Path, rel: &str) {
        let p = resolve(root, rel);
        not_a_link(&p);
        std::fs::remove_file(&p).unwrap_or_else(|e| panic!("remove {}: {e}", p.display()));
    }

    pub fn remove_dir(root: &Path, rel: &str) {
        let p = resolve(root, rel);
        not_a_link(&p);
        std::fs::remove_dir(&p).unwrap_or_else(|e| panic!("remove {}: {e}", p.display()));
    }

    pub fn try_rename(root: &Path, from: &str, to: &str) -> std::io::Result<()> {
        std::fs::rename(resolve(root, from), resolve(root, to))
    }

    pub fn try_remove_dir(root: &Path, rel: &str) -> std::io::Result<()> {
        std::fs::remove_dir(resolve(root, rel))
    }

    pub fn rename(root: &Path, from: &str, to: &str) {
        let (a, b) = (resolve(root, from), resolve(root, to));
        std::fs::rename(&a, &b).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", a.display(), b.display()));
    }

    pub fn touch(root: &Path, rel: &str, mtime: i64) {
        let w = verbatim(&resolve(root, rel));
        let h = open_raw(&w, FILE_WRITE_ATTRIBUTES.0).unwrap_or_else(|e| panic!("touch {rel}: {e}"));
        let ft = FILETIME { dwLowDateTime: mtime as u32, dwHighDateTime: (mtime >> 32) as u32 };
        // SAFETY: `h` is a valid handle with FILE_WRITE_ATTRIBUTES; `ft` lives for the call.
        unsafe { SetFileTime(HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&h)), None, None, Some(&ft)) }
            .unwrap_or_else(|e| panic!("touch {rel}: {e}"));
    }

    pub fn set_readonly(root: &Path, rel: &str, on: bool) {
        let w = verbatim(&resolve(root, rel));
        // SAFETY: `w` is NUL-terminated and lives for the call.
        let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
        assert!(attrs != INVALID_FILE_ATTRIBUTES, "set_readonly: {rel} does not exist");
        set_attrs(&w, if on { attrs | FILE_ATTRIBUTE_READONLY.0 } else { attrs & !FILE_ATTRIBUTE_READONLY.0 })
            .unwrap_or_else(|e| panic!("set_readonly {rel}: {e}"));
    }

    fn set_attrs(w: &[u16], attrs: u32) -> windows::core::Result<()> {
        let attrs = if attrs == 0 { FILE_ATTRIBUTE_NORMAL.0 } else { attrs };
        // SAFETY: `w` is NUL-terminated and lives for the call.
        unsafe { SetFileAttributesW(PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(attrs)) }
    }

    pub fn ads(root: &Path, rel: &str, stream: &str, bytes: &[u8]) {
        assert!(!stream.is_empty() && !stream.contains(['\\', '/', ':', '\0']), "ads: a plain stream name");
        let mut suffix = vec![u16::from(b':')];
        suffix.extend(stream.encode_utf16());
        create_raw(&with_suffix(&resolve(root, rel), &suffix), CREATE_ALWAYS, bytes);
    }

    /// Makes `rel` a mount-point junction to `target` (a folder inside a fenced root of `prefix`); no admin right needed.
    pub fn junction(root: &Path, rel: &str, target: &Path, prefix: &str) {
        assert!(
            within_fenced(target, prefix) && is_dir_exact(target),
            "junction target {} must be a folder inside a {prefix} root",
            target.display()
        );
        let link = resolve(root, rel);
        let w = verbatim(&link);
        // SAFETY: `w` is NUL-terminated and lives for the call.
        unsafe { CreateDirectoryW(PCWSTR(w.as_ptr()), None) }.unwrap_or_else(|e| panic!("junction {rel}: {e}"));
        let buf = mount_point_buffer(target);
        let set = open_raw(&w, GENERIC_WRITE.0).and_then(|h| {
            let mut returned = 0u32;
            // SAFETY: `h` is a valid handle to the new folder opened as itself; the buffer lives for the call and
            // its length is passed with it.
            unsafe {
                DeviceIoControl(
                    HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&h)),
                    FSCTL_SET_REPARSE_POINT,
                    Some(buf.as_ptr().cast::<c_void>()),
                    buf.len() as u32,
                    None,
                    0,
                    Some(&mut returned),
                    None,
                )
            }
        });
        if let Err(e) = set {
            let _ = std::fs::remove_dir(&link);
            panic!("junction {rel} -> {}: {e}", target.display());
        }
    }

    /// `p` is a folder, looked up by its exact name: a trailing dot or space is not trimmed (the \\?\ form).
    fn is_dir_exact(p: &Path) -> bool {
        let mut w = verbatim(p);
        w.pop();
        std::fs::metadata(PathBuf::from(std::ffi::OsString::from_wide(&w))).is_ok_and(|m| m.is_dir())
    }

    /// The REPARSE_DATA_BUFFER of a mount point: tag, data length, reserved, then the substitute name "\??\<target>"
    /// and the print name "<target>", each NUL-terminated.
    fn mount_point_buffer(target: &Path) -> Vec<u8> {
        let plain: Vec<u16> = target.as_os_str().encode_wide().collect();
        let mut subst: Vec<u16> = r"\??\".encode_utf16().collect();
        subst.extend_from_slice(&plain);
        let names = (subst.len() + 1 + plain.len() + 1) * 2;
        let data_len = 8 + names;
        let mut b = Vec::with_capacity(8 + data_len);
        b.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        b.extend((data_len as u16).to_le_bytes());
        b.extend(0u16.to_le_bytes());
        b.extend(0u16.to_le_bytes()); // substitute name offset
        b.extend(((subst.len() * 2) as u16).to_le_bytes());
        b.extend((((subst.len() + 1) * 2) as u16).to_le_bytes()); // print name offset
        b.extend(((plain.len() * 2) as u16).to_le_bytes());
        for u in subst.iter().chain([0u16].iter()).chain(plain.iter()).chain([0u16].iter()) {
            b.extend(u.to_le_bytes());
        }
        b
    }

    /// The print name of a mount point, read without following it.
    fn mount_point_target(p: &Path) -> Option<String> {
        let h = open_raw(&verbatim(p), 0).ok()?;
        let mut buf = vec![0u8; 16 * 1024];
        let mut returned = 0u32;
        // SAFETY: `h` is a valid handle opened as the link itself; the buffer is writable for its whole length.
        unsafe {
            DeviceIoControl(
                HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&h)),
                FSCTL_GET_REPARSE_POINT,
                None,
                0,
                Some(buf.as_mut_ptr().cast::<c_void>()),
                buf.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .ok()?;
        let u16_at = |i: usize| buf.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
        let tag = buf.get(0..4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))?;
        if tag != IO_REPARSE_TAG_MOUNT_POINT {
            return None;
        }
        let (off, len) = (usize::from(u16_at(12)?), usize::from(u16_at(14)?));
        let units: Vec<u16> =
            buf.get(16 + off..16 + off + len)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&units))
    }

    /// The entries of a folder ("." and ".." left out), read with FindFirstFileExW.
    pub fn list_dir(dir: &Path) -> std::io::Result<Vec<Found>> {
        let pattern = with_suffix(dir, &[u16::from(b'\\'), u16::from(b'*')]);
        let mut fd = WIN32_FIND_DATAW::default();
        // SAFETY: `pattern` is NUL-terminated; `fd` is a WIN32_FIND_DATAW, the type FindExInfoBasic fills.
        let h = match unsafe {
            FindFirstFileExW(
                PCWSTR(pattern.as_ptr()),
                FindExInfoBasic,
                (&mut fd as *mut WIN32_FIND_DATAW).cast::<c_void>(),
                FindExSearchNameMatch,
                None,
                FIND_FIRST_EX_FLAGS(0),
            )
        } {
            Ok(h) => h,
            Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(Vec::new()),
            Err(e) => return Err(std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF)),
        };
        struct Close(HANDLE);
        impl Drop for Close {
            fn drop(&mut self) {
                // SAFETY: a find handle from FindFirstFileExW, closed once.
                let _ = unsafe { FindClose(self.0) };
            }
        }
        let _close = Close(h);
        let mut out = Vec::new();
        loop {
            let n = fd.cFileName.iter().position(|&u| u == 0).unwrap_or(fd.cFileName.len());
            let name = fd.cFileName[..n].to_vec();
            if name != [u16::from(b'.')] && name != [u16::from(b'.'), u16::from(b'.')] {
                out.push(Found { name, attrs: fd.dwFileAttributes, reparse_tag: fd.dwReserved0 });
            }
            // SAFETY: `h` is the open find handle; `fd` is writable.
            if let Err(e) = unsafe { FindNextFileW(h, &mut fd) } {
                if e.code() == ERROR_NO_MORE_FILES.to_hresult() {
                    return Ok(out);
                }
                return Err(std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF));
            }
        }
    }

    /// Removes `dir` and everything under it without entering a reparse point; returns what could not be removed.
    pub fn remove_tree(dir: &Path) -> Vec<String> {
        let mut errors = Vec::new();
        remove_children(dir, &mut errors);
        let w = verbatim(dir);
        // SAFETY: `w` is NUL-terminated and lives for the call.
        let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
        if attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_READONLY.0 != 0 {
            let _ = set_attrs(&w, attrs & !FILE_ATTRIBUTE_READONLY.0);
        }
        // SAFETY: `w` is NUL-terminated and lives for the call.
        if let Err(e) = unsafe { RemoveDirectoryW(PCWSTR(w.as_ptr())) } {
            errors.push(format!("{}: {e}", dir.display()));
        }
        errors
    }

    fn remove_children(dir: &Path, errors: &mut Vec<String>) {
        let entries = match list_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("{}: {e}", dir.display()));
                return;
            }
        };
        for e in entries {
            let p = dir.join(OsString::from_wide(&e.name));
            let w = verbatim(&p);
            let link = e.attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
            let is_dir = e.attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
            if !link && e.attrs & FILE_ATTRIBUTE_READONLY.0 != 0 {
                let _ = set_attrs(&w, e.attrs & !FILE_ATTRIBUTE_READONLY.0);
            }
            if is_dir && !link {
                remove_children(&p, errors);
            }
            // A link is removed as a link: RemoveDirectoryW on a junction, DeleteFileW on a file symlink.
            let r = if is_dir {
                // SAFETY: `w` is NUL-terminated and lives for the call.
                unsafe { RemoveDirectoryW(PCWSTR(w.as_ptr())) }
            } else {
                // SAFETY: `w` is NUL-terminated and lives for the call.
                unsafe { DeleteFileW(PCWSTR(w.as_ptr())) }
            };
            if let Err(err) = r {
                errors.push(format!("{}: {err}", p.display()));
            }
        }
    }

    pub fn hash(root: &Path, rel: &str) -> u64 {
        let p = resolve(root, rel);
        not_a_link(&p);
        fnv1a64(&std::fs::read(&p).unwrap_or_else(|e| panic!("hash {}: {e}", p.display())))
    }

    pub fn hash_tree(root: &Path, rel: &str) -> BTreeMap<String, String> {
        let base = resolve(root, rel);
        let mut out = BTreeMap::new();
        walk_hash(&base, "", &mut out);
        out
    }

    fn walk_hash(dir: &Path, prefix: &str, out: &mut BTreeMap<String, String>) {
        let entries = list_dir(dir).unwrap_or_else(|e| panic!("hash_tree {}: {e}", dir.display()));
        for e in entries {
            let name = String::from_utf16_lossy(&e.name);
            let key = format!("{prefix}{name}");
            let p = dir.join(OsString::from_wide(&e.name));
            if e.attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                let value = match mount_point_target(&p) {
                    Some(t) => format!("junction -> {}", temp_relative(&t)),
                    None => format!("reparse {:#x}", e.reparse_tag),
                };
                out.insert(key, value);
            } else if e.attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
                out.insert(format!("{key}/"), "dir".to_string());
                walk_hash(&p, &format!("{key}/"), out);
            } else {
                let bytes = std::fs::read(&p).unwrap_or_else(|err| panic!("hash_tree {}: {err}", p.display()));
                out.insert(key.clone(), format!("file {:016x}", fnv1a64(&bytes)));
                for s in named_streams(&p) {
                    let mut sp = verbatim(&p);
                    sp.pop();
                    sp.push(u16::from(b':'));
                    sp.extend(s.encode_utf16());
                    let sb = std::fs::read(PathBuf::from(OsString::from_wide(&sp)))
                        .unwrap_or_else(|err| panic!("hash_tree {}:{s}: {err}", p.display()));
                    out.insert(format!("{key}:{s}"), format!("stream {:016x}", fnv1a64(&sb)));
                }
            }
        }
    }

    /// A junction target relative to %TEMP% ('/' separators), or the target itself when it lies elsewhere.
    fn temp_relative(target: &str) -> String {
        let t = target.trim_start_matches(r"\??\").trim_start_matches(r"\\?\");
        let lower = t.to_lowercase();
        let mut rest = t;
        // %TEMP% as given or in its long form (the roots' own form: temp_long).
        for b in [std::env::temp_dir(), temp_long()] {
            let base = b.to_string_lossy().trim_end_matches('\\').to_lowercase();
            if lower.starts_with(&format!("{base}\\")) {
                rest = &t[base.len() + 1..];
                break;
            }
        }
        rest.replace('\\', "/")
    }

    /// The names of the alternate data streams of a file (the unnamed main stream left out).
    fn named_streams(p: &Path) -> Vec<String> {
        let w = verbatim(p);
        let mut data = WIN32_FIND_STREAM_DATA::default();
        // SAFETY: `w` is NUL-terminated; `data` is the type FindStreamInfoStandard fills.
        let h = match unsafe {
            FindFirstStreamW(
                PCWSTR(w.as_ptr()),
                FindStreamInfoStandard,
                (&mut data as *mut WIN32_FIND_STREAM_DATA).cast::<c_void>(),
                None,
            )
        } {
            Ok(h) => h,
            Err(e) if e.code() == ERROR_HANDLE_EOF.to_hresult() => return Vec::new(),
            Err(e) => panic!("streams of {}: {e}", p.display()),
        };
        let mut out = Vec::new();
        loop {
            let n = data.cStreamName.iter().position(|&u| u == 0).unwrap_or(data.cStreamName.len());
            let full = String::from_utf16_lossy(&data.cStreamName[..n]);
            // ":name:$DATA"; the main stream is "::$DATA".
            if let Some(name) = full.strip_prefix(':').and_then(|r| r.strip_suffix(":$DATA")) {
                if !name.is_empty() {
                    out.push(name.to_string());
                }
            }
            // SAFETY: `h` is the open stream-find handle; `data` is writable.
            if unsafe { FindNextStreamW(h, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast::<c_void>()) }.is_err() {
                break;
            }
        }
        // SAFETY: a find handle from FindFirstStreamW, closed once.
        let _ = unsafe { FindClose(h) };
        out.sort();
        out
    }

    /// The current user's SID as a string.
    fn user_sid() -> String {
        let mut token = HANDLE::default();
        // SAFETY: the current process pseudo-handle; `token` receives a handle we close below.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.expect("open the process token");
        // SAFETY: a fresh token handle; the OwnedHandle closes it.
        let _owned = unsafe { OwnedHandle::from_raw_handle(token.0) };
        let mut len = 0u32;
        // SAFETY: a size query: no buffer, `len` receives the size.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
        assert!(len as usize >= std::mem::size_of::<TOKEN_USER>(), "TokenUser size");
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` holds at least `len` writable, pointer-aligned bytes.
        unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len) }
            .expect("TokenUser");
        // SAFETY: the buffer starts with a TOKEN_USER whose SID points inside `buf`, alive until the end.
        let tu = unsafe { std::ptr::read(buf.as_ptr().cast::<TOKEN_USER>()) };
        let mut s = PWSTR::null();
        // SAFETY: `tu.User.Sid` points to a valid SID inside `buf`; `s` receives a LocalAlloc'd string.
        unsafe { ConvertSidToStringSidW(tu.User.Sid, &mut s) }.expect("ConvertSidToStringSidW");
        // SAFETY: `s` is the NUL-terminated string returned above, read once.
        let out = unsafe { s.to_string() }.expect("a SID string");
        // SAFETY: allocated with LocalAlloc by ConvertSidToStringSidW; not used afterwards.
        unsafe { LocalFree(Some(HLOCAL(s.0.cast()))) };
        out
    }

    /// Sets the explicit part of a folder's DACL from SDDL; inherited entries stay (an unprotected DACL).
    fn set_dacl(p: &Path, sddl: &str) {
        let wide: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor freed below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(wide.as_ptr()), SDDL_REVISION_1, &mut sd, None)
        }
        .unwrap_or_else(|e| panic!("SDDL {sddl}: {e}"));
        let (mut present, mut defaulted) = (windows::core::BOOL(0), windows::core::BOOL(0));
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `sd` is the valid descriptor made above; the out pointers are valid.
        unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) }
            .expect("the DACL of the SDDL");
        let w = verbatim(p);
        // SAFETY: `w` is NUL-terminated; `dacl` points into `sd`, alive until LocalFree below.
        let err = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(w.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl.cast_const()),
                None,
            )
        };
        // SAFETY: allocated with LocalAlloc by the conversion above; not used afterwards.
        unsafe { LocalFree(Some(HLOCAL(sd.0))) };
        assert!(err.is_ok(), "SetNamedSecurityInfoW {}: {err:?}", p.display());
    }

    /// Denies this user FILE_LIST_DIRECTORY on the folder `p`.
    pub fn deny_listing_at(p: &Path) {
        set_dacl(p, &format!("D:(D;;0x1;;;{})", user_sid()));
    }

    /// Drops every explicit entry of the folder's DACL: the inherited ones remain, as when it was created.
    pub fn allow_listing_at(p: &Path) {
        set_dacl(p, "D:");
    }
}
