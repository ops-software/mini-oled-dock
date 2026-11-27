//! The app's own data folder, %LOCALAPPDATA%\mini-oled-dock\files (FILES_DATA_DIR overrides it in a debug build, so
//! every test child gets its own folder and no test process touches the real one).
//!
//! This is the only file of the package's code where files.exe itself writes, renames or deletes a file (lint
//! mutations_go_through_ifileoperation); user files change only in the ops host, through IFileOperation.
//!
//! Every method takes a path relative to the folder and refuses one that is absolute, holds a "." or ".." component,
//! a drive or stream colon, a control character or a component Win32 would trim (a trailing dot or space), one that
//! resolves outside the folder (GetFullPathNameW, case-insensitive prefix compare), and one that crosses a reparse
//! point (a junction or a symbolic link) at or under the folder. The last component is opened without following a
//! reparse point and refused when it is one. A process of the same user could still swap a component between the
//! check and the use; it holds the same rights as files.exe, so the fence guards against our own mistakes and against
//! links planted in the folder, not against the user.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::{
    GetFullPathNameW, MoveFileExW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_OPEN_REPARSE_POINT, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// The suite's folder under %LOCALAPPDATA%, and this app's folder inside it.
const SUITE_DIR: &str = "mini-oled-dock";
const APP_DIR: &str = "files";

/// Debug builds only: the data folder of a test child (TestHooks.env sets it).
const DATA_DIR_VAR: &str = "FILES_DATA_DIR";

/// The app's data folder. Created on the first write; nothing is created by `default_dir`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnDir(PathBuf);

impl OwnDir {
    /// %LOCALAPPDATA%\mini-oled-dock\files, or FILES_DATA_DIR in a debug build (an absolute path, else none).
    pub fn default_dir() -> Option<OwnDir> {
        pick_dir(cfg!(debug_assertions), std::env::var_os(DATA_DIR_VAR), local_app_data).map(OwnDir)
    }

    /// The folder at `path` (absolute), created when absent; refused when it is a reparse point or not a folder.
    pub fn open(path: &Path) -> io::Result<OwnDir> {
        if !path.is_absolute() {
            return Err(refused("the data folder must be an absolute path"));
        }
        let d = OwnDir(path.to_path_buf());
        d.ensure_root()?;
        Ok(d)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Replaces (or creates) `rel` atomically: a temporary file in the same folder, flushed, then renamed over the old
    /// one with MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH. Missing parent folders are created.
    pub fn write_atomic(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let target = self.checked(rel, Need::Parents)?;
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let tmp = target.with_file_name(format!(".{name}.{}-{n}.tmp", std::process::id()));
        let result = (|| {
            // create_new: nothing may exist under the temporary name, not even a link.
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?; // FlushFileBuffers
            drop(f);
            let (from, to) = (HSTRING::from(tmp.as_os_str()), HSTRING::from(target.as_os_str()));
            // SAFETY: both strings are NUL-terminated and live for the call.
            unsafe { MoveFileExW(&from, &to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
                .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    /// Opens `rel` for appending, created when absent (missing parent folders too).
    pub fn append(&self, rel: &str) -> io::Result<File> {
        let target = self.checked(rel, Need::Parents)?;
        let f =
            OpenOptions::new().append(true).create(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0).open(&target)?;
        refuse_link_or_dir(&f)?;
        Ok(f)
    }

    /// The bytes of `rel`.
    pub fn read(&self, rel: &str) -> io::Result<Vec<u8>> {
        let target = self.checked(rel, Need::Existing)?;
        let mut f = OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0).open(&target)?;
        refuse_link_or_dir(&f)?;
        let mut out = Vec::new();
        f.read_to_end(&mut out)?;
        Ok(out)
    }

    /// The names in the folder `rel_dir` ("" is the data folder itself).
    pub fn list(&self, rel_dir: &str) -> io::Result<Vec<OsString>> {
        let dir = if rel_dir.is_empty() {
            self.ensure_root()?;
            self.0.clone()
        } else {
            self.checked(rel_dir, Need::Existing)?
        };
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dir)? {
            out.push(e?.file_name());
        }
        out.sort();
        Ok(out)
    }

    /// Removes the file `rel` (never a folder, never a link).
    pub fn remove(&self, rel: &str) -> io::Result<()> {
        let target = self.checked(rel, Need::Existing)?;
        let m = std::fs::symlink_metadata(&target)?;
        if m.file_attributes() & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
            return Err(refused("remove takes a file, not a folder"));
        }
        std::fs::remove_file(&target)
    }

    /// The folder exists, is a folder and is not a reparse point.
    fn ensure_root(&self) -> io::Result<()> {
        match std::fs::symlink_metadata(&self.0) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::create_dir_all(&self.0)?,
            Err(e) => return Err(e),
        }
        let m = std::fs::symlink_metadata(&self.0)?;
        if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(refused("the data folder is a reparse point"));
        }
        if !m.is_dir() {
            return Err(refused("the data folder is not a folder"));
        }
        Ok(())
    }

    /// Checks `rel` and returns its full path. Every existing component from the folder down to the target must not
    /// be a reparse point; with Need::Parents the missing parent folders are created one by one.
    fn checked(&self, rel: &str, need: Need) -> io::Result<PathBuf> {
        let parts = components(rel)?;
        self.ensure_root()?;
        if !is_inside(&self.0, &parts.iter().fold(self.0.clone(), |p, c| p.join(c)))? {
            return Err(refused("the path resolves outside the data folder"));
        }
        let mut path = self.0.clone();
        for (i, part) in parts.iter().enumerate() {
            path.push(part);
            let last = i + 1 == parts.len();
            match std::fs::symlink_metadata(&path) {
                Ok(m) => {
                    if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                        return Err(refused("the path crosses a reparse point"));
                    }
                    if !last && !m.is_dir() {
                        return Err(refused("a parent of the path is not a folder"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => match need {
                    Need::Existing => return Err(e),
                    Need::Parents if !last => std::fs::create_dir(&path)?,
                    Need::Parents => {}
                },
                Err(e) => return Err(e),
            }
        }
        Ok(path)
    }
}

/// What `checked` does with missing components.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    Existing,
    Parents,
}

/// Appends to a log file of the data folder: only paths under `OwnDir::default_dir()` are opened.
pub fn open_log(path: &Path) -> Option<File> {
    let d = OwnDir::default_dir()?;
    let rel = relative_to(d.path(), path)?;
    d.append(&rel).ok()
}

/// The choice of the data folder (pure): the override in a debug build when it is absolute, else LocalAppData's.
fn pick_dir(debug: bool, over: Option<OsString>, local: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    if debug {
        if let Some(v) = over.filter(|v| !v.is_empty()) {
            let p = PathBuf::from(v);
            return p.is_absolute().then_some(p);
        }
    }
    local().map(|l| l.join(SUITE_DIR).join(APP_DIR))
}

fn local_app_data() -> Option<PathBuf> {
    // SAFETY: a known folder id and default flags; the returned string is freed below.
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: `p` is the NUL-terminated string returned by the call above, read once.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: allocated with CoTaskMemAlloc by SHGetKnownFolderPath; not used afterwards.
    unsafe { CoTaskMemFree(Some(p.0 as *const std::ffi::c_void)) };
    s.filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// The components of a relative path, or the reason it is refused.
fn components(rel: &str) -> io::Result<Vec<&str>> {
    if rel.is_empty() {
        return Err(refused("empty path"));
    }
    if rel.contains(':') {
        return Err(refused("a drive or stream colon"));
    }
    if rel.starts_with(['\\', '/']) {
        return Err(refused("an absolute path"));
    }
    if rel.chars().any(|c| (c as u32) < 0x20) {
        return Err(refused("a control character"));
    }
    let parts: Vec<&str> = rel.split(['\\', '/']).collect();
    for p in &parts {
        if p.is_empty() || *p == "." || *p == ".." {
            return Err(refused("an empty, \".\" or \"..\" component"));
        }
        if p.ends_with(['.', ' ']) {
            return Err(refused("a component Win32 would trim"));
        }
    }
    Ok(parts)
}

/// `path` lies strictly inside `root` once both are resolved (case-insensitive).
fn is_inside(root: &Path, path: &Path) -> io::Result<bool> {
    let root = full_path(root)?.to_lowercase();
    let path = full_path(path)?.to_lowercase();
    let prefix = format!("{}\\", root.trim_end_matches('\\'));
    Ok(path.starts_with(&prefix) && path.len() > prefix.len())
}

/// `path` relative to `root` when it lies inside it.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let root_full = full_path(root).ok()?;
    let path_full = full_path(path).ok()?;
    let prefix = format!("{}\\", root_full.trim_end_matches('\\'));
    let lower = path_full.to_lowercase();
    if !lower.starts_with(&prefix.to_lowercase()) || path_full.len() <= prefix.len() {
        return None;
    }
    path_full.get(prefix.len()..).map(str::to_string)
}

/// GetFullPathNameW of `p` (no file system access: "." and ".." are folded, nothing is followed).
fn full_path(p: &Path) -> io::Result<String> {
    let wide = HSTRING::from(p.as_os_str());
    let mut buf = vec![0u16; 520];
    loop {
        // SAFETY: `wide` is NUL-terminated; the buffer is writable for its whole length.
        let n = unsafe { GetFullPathNameW(&wide, Some(&mut buf), None) } as usize;
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        if n < buf.len() {
            buf.truncate(n);
            return Ok(String::from_utf16_lossy(&buf));
        }
        buf.resize(n + 1, 0);
    }
}

/// Refuses an opened handle that turned out to be a link (opened without following it) or a folder.
fn refuse_link_or_dir(f: &File) -> io::Result<()> {
    let attrs = f.metadata()?.file_attributes();
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(refused("the file is a reparse point"));
    }
    if attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
        return Err(refused("the path is a folder"));
    }
    Ok(())
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, format!("own data folder: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempRoot;

    fn own(root: &TempRoot) -> OwnDir {
        OwnDir::open(&root.dir("own")).expect("open the data folder")
    }

    #[test]
    fn refuses_absolute() {
        let root = TempRoot::new("own_abs");
        let d = own(&root);
        for rel in ["C:\\x.txt", "\\x.txt", "/x.txt", "\\\\srv\\share\\x.txt"] {
            assert!(d.write_atomic(rel, b"x").is_err(), "{rel}");
            assert!(d.append(rel).is_err(), "{rel}");
            assert!(d.read(rel).is_err(), "{rel}");
            assert!(d.remove(rel).is_err(), "{rel}");
        }
    }

    #[test]
    fn refuses_dotdot() {
        let root = TempRoot::new("own_dotdot");
        root.file("outside.txt", b"keep");
        let d = own(&root);
        for rel in ["..\\outside.txt", "a\\..\\..\\outside.txt", ".", "..", "a\\.\\b", "a\\\\b", "a.\\b", "b "] {
            assert!(d.write_atomic(rel, b"x").is_err(), "{rel}");
            assert!(d.remove(rel).is_err(), "{rel}");
            assert!(d.list(rel).is_err(), "{rel}");
        }
        assert_eq!(root.hash("outside.txt"), crate::testkit::fnv1a64(b"keep"));
    }

    #[test]
    fn refuses_stream_colon() {
        let root = TempRoot::new("own_colon");
        let d = own(&root);
        d.write_atomic("a.txt", b"main").unwrap();
        for rel in ["a.txt:s", "a.txt::$DATA", "a.txt:s:$DATA", "c:a.txt"] {
            assert!(d.write_atomic(rel, b"x").is_err(), "{rel}");
            assert!(d.append(rel).is_err(), "{rel}");
            assert!(d.read(rel).is_err(), "{rel}");
        }
        assert_eq!(d.read("a.txt").unwrap(), b"main");
    }

    #[test]
    fn refuses_junction_inside() {
        let root = TempRoot::new("own_junction");
        let other = TempRoot::new("own_junction_target");
        other.file("data\\t.txt", b"target");
        let before = other.hash_tree("");
        let d = own(&root);
        root.junction("own\\j", &other.path().join("data"));
        assert!(d.write_atomic("j\\t.txt", b"x").is_err());
        assert!(d.write_atomic("j\\new.txt", b"x").is_err());
        assert!(d.append("j\\log.txt").is_err());
        assert!(d.read("j\\t.txt").is_err());
        assert!(d.list("j").is_err());
        assert!(d.remove("j\\t.txt").is_err());
        assert!(d.write_atomic("j", b"x").is_err(), "the link itself is refused too");
        assert_eq!(other.hash_tree(""), before, "the junction's target is unchanged");
    }

    #[test]
    fn open_refuses_a_junction_root() {
        let root = TempRoot::new("own_root_junction");
        let other = TempRoot::new("own_root_target");
        other.dir("data");
        root.junction("j", &other.path().join("data"));
        assert!(OwnDir::open(&root.path().join("j")).is_err());
        assert!(OwnDir::open(Path::new("relative\\dir")).is_err());
    }

    #[test]
    fn write_atomic_replaces() {
        let root = TempRoot::new("own_atomic");
        let d = own(&root);
        d.write_atomic("a\\b.json", b"one").unwrap();
        d.write_atomic("a\\b.json", b"two").unwrap();
        assert_eq!(d.read("a\\b.json").unwrap(), b"two");
        assert_eq!(d.list("a").unwrap(), vec![OsString::from("b.json")], "no temporary file is left");
        d.remove("a\\b.json").unwrap();
        assert!(d.list("a").unwrap().is_empty());
        assert!(d.remove("a").is_err(), "remove takes files only");
    }

    #[test]
    fn append_appends() {
        let root = TempRoot::new("own_append");
        let d = own(&root);
        d.append("logs\\x.log").unwrap().write_all(b"a").unwrap();
        d.append("logs\\x.log").unwrap().write_all(b"b").unwrap();
        assert_eq!(d.read("logs\\x.log").unwrap(), b"ab");
        assert_eq!(d.list("").unwrap(), vec![OsString::from("logs")]);
    }

    #[test]
    fn pick_dir_table() {
        let local = || Some(PathBuf::from("C:\\L"));
        let want_local = Some(PathBuf::from("C:\\L\\mini-oled-dock\\files"));
        assert_eq!(pick_dir(false, None, local), want_local);
        assert_eq!(pick_dir(false, Some("D:\\t".into()), local), want_local, "release ignores the override");
        assert_eq!(pick_dir(true, Some("D:\\t".into()), local), Some(PathBuf::from("D:\\t")));
        assert_eq!(pick_dir(true, Some("rel\\t".into()), local), None, "a relative override is refused");
        assert_eq!(pick_dir(true, Some("".into()), local), want_local);
        assert_eq!(pick_dir(true, None, || None), None);
    }

    #[test]
    fn open_log_refuses_paths_outside_the_data_folder() {
        let root = TempRoot::new("own_log");
        assert!(open_log(&root.path().join("x.log")).is_none());
        assert!(root.hash_tree("").is_empty(), "nothing was created");
    }

    #[test]
    fn relative_to_and_is_inside() {
        let r = Path::new("C:\\Data\\Files");
        assert_eq!(relative_to(r, Path::new("c:\\data\\files\\a\\b.log")).as_deref(), Some("a\\b.log"));
        assert_eq!(relative_to(r, Path::new("C:\\Data\\FilesX\\b.log")), None);
        assert_eq!(relative_to(r, Path::new("C:\\Data\\Files")), None);
        assert_eq!(relative_to(r, Path::new("C:\\Data\\Files\\..\\x.log")), None);
        assert!(is_inside(r, Path::new("C:\\DATA\\files\\x")).unwrap());
        assert!(!is_inside(r, Path::new("C:\\Data\\x")).unwrap());
    }
}
