//! The Recycle Bin's $I records of the items a restore moves out of the bin. Moving an item out leaves its $I record
//! behind as an orphan; the runner removes that record, and only that one: it pairs with the item by name, the item
//! has left the bin, and the record, opened as itself, is a plain file that holds the path the item went back to. The
//! check and the delete go through one handle, so a name swapped between them changes nothing. This is the only file
//! of the ops host that deletes a file without IFileOperation (lint mutations_go_through_ifileoperation):
//! IFileOperation deletes by name, and an $I record is the bin's own bookkeeping, never a user's file.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;

use oled_trust::frame::files::WidePath;
use windows::core::HRESULT;
use windows::Win32::Foundation::{E_FAIL, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FileDispositionInfo, SetFileInformationByHandle, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE,
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_READ, FILE_SHARE_READ,
};

use super::checks::{folded, same_path};

/// The $I record of a recycled item: `bin` is $R<id> directly inside a user's folder (S-1-...) of a $Recycle.Bin
/// folder, and its record is $I<id> beside it. None for any other path. The bin is known by those two folder names, so
/// a test can build one anywhere; each volume's bin holds its own records, and nothing outside `bin`'s folder is named.
pub fn bin_record(bin: &WidePath) -> Option<WidePath> {
    let rest = bin.file_name()?.strip_prefix(&[u16::from(b'$'), u16::from(b'R')]).filter(|r| !r.is_empty())?;
    let user = bin.parent()?;
    let top = user.parent()?;
    let units = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let in_bin = folded(user.file_name()?).starts_with(&units("S-1-"))
        && folded(top.file_name()?) == folded(&units("$Recycle.Bin"));
    if !in_bin {
        return None;
    }
    let mut name = vec![u16::from(b'$'), u16::from(b'I')];
    name.extend_from_slice(rest);
    user.join(&name).ok()
}

/// The size of the largest $I record: the version 2 header and 32,768 units.
const RECORD_MAX: u64 = 28 + 2 * 32_768;

/// The original path an $I record holds: version 1 (Windows Vista to 8.1) keeps 260 units from byte 24; version 2
/// (Windows 10 and later) the count of units (the NUL included) at byte 24 and the units from 28. None for anything
/// else: another version or length, no NUL, an empty path.
fn record_path(b: &[u8]) -> Option<Vec<u16>> {
    let version = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
    let units = match version {
        1 if b.len() == 24 + 2 * 260 => b.get(24..)?,
        2 => {
            let n = u32::from_le_bytes(b.get(24..28)?.try_into().ok()?);
            if b.len() as u64 != 28 + 2 * u64::from(n) {
                return None;
            }
            b.get(28..)?
        }
        _ => return None,
    };
    let mut w: Vec<u16> =
        units.chunks_exact(2).map(|c| u16::from_le_bytes(<[u8; 2]>::try_from(c).unwrap_or_default())).collect();
    let end = w.iter().position(|&u| u == 0).filter(|&n| n > 0)?;
    w.truncate(end);
    Some(w)
}

/// The attributes an $I record never has: a folder, a link, or a placeholder whose data lives elsewhere (reading it
/// would fetch it).
const NOT_A_RECORD: u32 = FILE_ATTRIBUTE_DIRECTORY.0
    | FILE_ATTRIBUTE_REPARSE_POINT.0
    | FILE_ATTRIBUTE_OFFLINE.0
    | FILE_ATTRIBUTE_RECALL_ON_OPEN.0
    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0;

/// Whether an opened file is the $I record of an item restored to one of `restored` (where it was deleted from, as the
/// undo record and the shell name it), from what its handle reports: never a folder, a link or a placeholder, at most
/// RECORD_MAX bytes, and the path its bytes hold is one of `restored`. `read` gives the bytes; it is called only once
/// the attributes and the length pass, so nothing but a plain file is ever read. Pure.
pub fn is_own_record(attributes: u32, len: u64, read: impl FnOnce() -> Option<Vec<u8>>, restored: &[WidePath]) -> bool {
    if attributes & NOT_A_RECORD != 0 || len > RECORD_MAX {
        return false;
    }
    let held = read().and_then(|b| record_path(&b)).and_then(|w| WidePath::new(&w).ok());
    held.is_some_and(|h| restored.iter().any(|r| same_path(&h, r)))
}

/// What became of the $I record of an item a restore moved out of the Recycle Bin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forgot {
    /// Removed: it was that item's own orphan.
    Removed,
    /// Left as it is: no record, the item still in the bin, or not that item's own (a folder, a link, a placeholder,
    /// not a record, another path).
    Kept,
    /// Still there: it could not be opened to be checked, or not removed. The HRESULT.
    Failed(i32),
}

/// Removes `rec`, the record bin_record pairs with `bin`, once `bin` has left the bin and the record proves to be its
/// own (is_own_record). One handle does it all: the record is opened as itself (never through a link), and nobody else
/// may write, rename or delete it while it is open; its facts are read through that handle, `between` is called (a
/// test seam), and the delete goes through the same handle. So what is removed is the very file that was checked,
/// whatever happens to its name meanwhile: IFileOperation deletes by name and cannot promise that.
pub fn forget_record(bin: &WidePath, rec: &WidePath, restored: &[WidePath], between: &dyn Fn(&WidePath)) -> Forgot {
    if super::sink::attrs(bin).is_some() {
        return Forgot::Kept;
    }
    let raw = rec.raw();
    let path = PathBuf::from(OsString::from_wide(raw.strip_suffix(&[0]).unwrap_or(&raw)));
    let opened = OpenOptions::new()
        .access_mode(FILE_GENERIC_READ.0 | DELETE.0)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags((FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_NO_RECALL).0)
        .open(&path);
    let mut f = match opened {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Forgot::Kept,
        Err(e) => return Forgot::Failed(e.raw_os_error().map_or(E_FAIL, |c| HRESULT::from_win32(c as u32)).0),
    };
    let Ok(meta) = f.metadata() else { return Forgot::Kept };
    let read = || {
        let mut b = Vec::new();
        f.by_ref().take(RECORD_MAX + 1).read_to_end(&mut b).ok().map(|_| b)
    };
    if !is_own_record(meta.file_attributes(), meta.len(), read, restored) {
        return Forgot::Kept;
    }
    between(rec);
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `f` is open with DELETE access for the call; `info` lives for the call and its size is passed with it.
    let set = unsafe {
        SetFileInformationByHandle(
            HANDLE(f.as_raw_handle()),
            FileDispositionInfo,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    match set {
        Ok(()) => Forgot::Removed,
        Err(e) => Forgot::Failed(e.code().0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> WidePath {
        WidePath::parse(s).unwrap()
    }

    /// An $I record of `version` naming `path`: version 1 keeps 260 units, version 2 counts them (the NUL too).
    fn record(version: u64, path: &[u16]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(version.to_le_bytes());
        b.extend(5u64.to_le_bytes());
        b.extend(134_080_000_000_000_000i64.to_le_bytes());
        let mut w = path.to_vec();
        if version == 1 {
            w.resize(260, 0);
        } else {
            w.push(0);
            b.extend((w.len() as u32).to_le_bytes());
        }
        w.iter().for_each(|u| b.extend(u.to_le_bytes()));
        b
    }

    #[test]
    fn bin_records_pair_by_name() {
        let sid = r"C:\$Recycle.Bin\S-1-5-21-1000-2000-3000-1001";
        let pairs = [
            (format!(r"{sid}\$RAB12CD.txt"), format!(r"{sid}\$IAB12CD.txt")),
            (format!(r"{sid}\$RAB12CD"), format!(r"{sid}\$IAB12CD")),
            (r"D:\$RECYCLE.BIN\S-1-5-21-9\$RX1.tar.gz".into(), r"D:\$RECYCLE.BIN\S-1-5-21-9\$IX1.tar.gz".into()),
            (r"E:\t\vol\$Recycle.Bin\S-1-5-18\$R1".into(), r"E:\t\vol\$Recycle.Bin\S-1-5-18\$I1".into()),
        ];
        for (bin, rec) in &pairs {
            assert_eq!(bin_record(&p(bin)), Some(p(rec)), "{bin}");
        }
        let others = [
            format!(r"{sid}\$IAB12CD.txt"),
            format!(r"{sid}\$R"),
            format!(r"{sid}\$rAB12CD.txt"),
            format!(r"{sid}\desktop.ini"),
            format!(r"{sid}\$RAB12CD\$RINNER.txt"),
            r"C:\$Recycle.Bin\$RAB12CD.txt".into(),
            r"C:\Recycle\S-1-5-21-1\$RAB12CD.txt".into(),
            r"C:\$Recycle.Bin\X-1-5-21\$RAB12CD.txt".into(),
            r"C:\docs\$RAB12CD.txt".into(),
        ];
        for bin in &others {
            assert_eq!(bin_record(&p(bin)), None, "{bin}");
        }
    }

    #[test]
    fn records_hold_the_original_path() {
        let path: Vec<u16> = r"C:\docs\a.txt".encode_utf16().collect();
        assert_eq!(record_path(&record(2, &path)), Some(path.clone()), "Windows 10 and later");
        assert_eq!(record_path(&record(1, &path)), Some(path.clone()), "Windows Vista to 8.1");
        let v2 = record(2, &path);
        let mut wrong = v2.clone();
        wrong[0] = 3;
        let mut cut = v2.clone();
        cut.pop();
        let mut more = v2.clone();
        more.extend([0, 0]);
        let mut open = v2.clone();
        let n = open.len();
        open[n - 2] = b'x';
        let (v1, empty) = (record(1, &path), record(2, &[]));
        for (bytes, what) in [
            (&wrong[..], "an unknown version"),
            (&cut[..], "shorter than its count"),
            (&more[..], "longer than its count"),
            (&open[..], "no NUL"),
            (&v1[..543], "a version 1 record is 544 bytes"),
            (&empty[..], "an empty path"),
            (&[][..], "nothing"),
        ] {
            assert_eq!(record_path(bytes), None, "{what}");
        }
    }

    #[test]
    fn own_records_are_plain_files_that_name_a_restored_path() {
        let path: Vec<u16> = r"C:\docs\a.txt".encode_utf16().collect();
        let restored = [p(r"C:\docs\b.txt"), p(r"c:\DOCS\A.TXT")];
        let bytes = record(2, &path);
        let (len, read) = (bytes.len() as u64, || Some(bytes.clone()));
        let plain = windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_ARCHIVE.0;
        assert!(is_own_record(plain, len, read, &restored), "a plain file that names a restored path");
        assert!(is_own_record(0, len, read, &restored[1..]), "no attributes at all");
        assert!(!is_own_record(plain, len, read, &restored[..1]), "another path");
        assert!(!is_own_record(plain, len, || None, &restored), "unreadable");
        assert!(!is_own_record(plain, 12, || Some(b"not a record".to_vec()), &restored), "not a record");
        // Nothing but a plain file of a record's size is ever read.
        let never = || -> Option<Vec<u8>> { panic!("read before the handle's facts were checked") };
        for (a, what) in [
            (FILE_ATTRIBUTE_DIRECTORY, "a folder"),
            (FILE_ATTRIBUTE_REPARSE_POINT, "a link or a junction"),
            (FILE_ATTRIBUTE_OFFLINE, "data kept offline"),
            (FILE_ATTRIBUTE_RECALL_ON_OPEN, "a placeholder"),
            (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, "a cloud placeholder"),
        ] {
            assert!(!is_own_record(plain | a.0, len, never, &restored), "{what}");
        }
        assert!(!is_own_record(plain, RECORD_MAX + 1, never, &restored), "longer than any record");
    }
}
