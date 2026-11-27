//! Enumeration: the Enumerator trait and FindEnumerator (F-ENUM fills run), and stat_one, complete here because the
//! ops pre-flight and the watcher's resolve call it in their own tests.

use std::ffi::c_void;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_NETPATH, ERROR_BAD_NET_NAME, ERROR_DIRECTORY, ERROR_FILENAME_EXCED_RANGE,
    ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_NETNAME_DELETED, ERROR_NETWORK_UNREACHABLE,
    ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND, ERROR_UNEXP_NET_ERR,
};
use windows::Win32::Storage::FileSystem::{
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FILE_ATTRIBUTE_REPARSE_POINT,
    FIND_FIRST_EX_FLAGS, WIN32_FIND_DATAW,
};

use super::types::{Batch, CancelToken, EnumError, RawEntry};
use super::{Gen, WidePath};

pub trait Enumerator: Send {
    fn run(&mut self, dir: &WidePath, gen: Gen, cancel: &CancelToken, out: &mut dyn FnMut(Batch));
}

pub struct FindEnumerator {
    pub first_entries: usize,
    pub first_ms: u32,
    pub later_entries: usize,
    pub later_ms: u32,
} // Default: 256, 8, 2048, 16

impl Default for FindEnumerator {
    fn default() -> Self {
        FindEnumerator { first_entries: 256, first_ms: 8, later_entries: 2048, later_ms: 16 }
    }
}

impl Enumerator for FindEnumerator {
    fn run(&mut self, dir: &WidePath, gen: Gen, cancel: &CancelToken, out: &mut dyn FnMut(Batch)) {
        let _ = (dir, gen, cancel, out);
        todo!("F-ENUM")
    }
}

pub struct OwnedEntry {
    pub name: Box<[u8]>,
    pub e: RawEntry,
} // e.name_off = 0

/// The entry named `name` (WTF-8) in `dir`, read with FindFirstFileExW on that name; Ok(None) = no such name. A
/// name that cannot exist on NTFS (empty, ".", "..", a NUL or control character, or one of \ / : * ? " < > |) is
/// Ok(None) without a call. The match is the file system's: a name that differs only in case, or the 8.3 short name
/// of an entry, finds that entry and returns it under its stored long name (a create, copy or move to `name` would
/// hit it).
pub fn stat_one(dir: &WidePath, name: &[u8]) -> Result<Option<OwnedEntry>, EnumError> {
    let mut units = Vec::with_capacity(name.len());
    if !wtf8_to_utf16(name, &mut units) || !is_plain_name(&units) {
        return Ok(None);
    }
    let mut path = dir.raw();
    path.pop(); // the NUL
    if path.last() != Some(&u16::from(b'\\')) {
        path.push(u16::from(b'\\'));
    }
    path.extend_from_slice(&units);
    path.push(0);
    let mut fd = WIN32_FIND_DATAW::default();
    let start = Instant::now();
    // SAFETY: `path` is NUL-terminated and lives for the call; `fd` is the WIN32_FIND_DATAW FindExInfoBasic fills.
    let found = unsafe {
        FindFirstFileExW(
            PCWSTR(path.as_ptr()),
            FindExInfoBasic,
            (&mut fd as *mut WIN32_FIND_DATAW).cast::<c_void>(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_FLAGS(0),
        )
    };
    match found {
        Ok(h) => {
            // SAFETY: the find handle returned above, closed once.
            let _ = unsafe { FindClose(h) };
        }
        Err(e) => {
            let code = win32_code(&e);
            if code == ERROR_FILE_NOT_FOUND.0 || code == ERROR_PATH_NOT_FOUND.0 {
                return Ok(None);
            }
            let waited_ms = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
            return Err(map_find_error(code, waited_ms));
        }
    }
    let mut names = Vec::with_capacity(name.len());
    let e = raw_from_find(&fd, &mut names);
    Ok(Some(OwnedEntry { name: names.into_boxed_slice(), e }))
}

/// One find record as a RawEntry; its name (up to the NUL, as WTF-8) is appended to `names`. EN1 reuses it.
pub(crate) fn raw_from_find(d: &WIN32_FIND_DATAW, names: &mut Vec<u8>) -> RawEntry {
    let name_off = names.len() as u32;
    utf16_to_wtf8(until_nul(&d.cFileName), names);
    let name_len = (names.len() - name_off as usize).min(usize::from(u16::MAX)) as u16;
    let reparse = d.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
    let filetime =
        |t: windows::Win32::Foundation::FILETIME| (i64::from(t.dwHighDateTime) << 32) | i64::from(t.dwLowDateTime);
    RawEntry {
        name_off,
        name_len,
        attrs: d.dwFileAttributes,
        reparse_tag: if reparse { d.dwReserved0 } else { 0 },
        size: (u64::from(d.nFileSizeHigh) << 32) | u64::from(d.nFileSizeLow),
        mtime: filetime(d.ftLastWriteTime),
        ctime: filetime(d.ftCreationTime),
    }
}

/// The error of a failed FindFirstFileExW (EN1's table). Not found on the name itself is the caller's Ok(None).
pub(crate) fn map_find_error(code: u32, waited_ms: u32) -> EnumError {
    let offline =
        [ERROR_BAD_NETPATH, ERROR_BAD_NET_NAME, ERROR_NETNAME_DELETED, ERROR_UNEXP_NET_ERR, ERROR_NETWORK_UNREACHABLE];
    match code {
        c if c == ERROR_ACCESS_DENIED.0 => EnumError::AccessDenied,
        c if c == ERROR_PATH_NOT_FOUND.0 || c == ERROR_DIRECTORY.0 => EnumError::NotFound,
        c if offline.iter().any(|e| e.0 == c) => EnumError::Offline { waited_ms },
        c if c == ERROR_FILENAME_EXCED_RANGE.0 => EnumError::TooLong,
        c if c == ERROR_INVALID_FUNCTION.0 || c == ERROR_NOT_SUPPORTED.0 => EnumError::Unsupported,
        c => EnumError::Win32(c),
    }
}

/// The Win32 code inside an HRESULT_FROM_WIN32, else the HRESULT's bits.
fn win32_code(e: &windows::core::Error) -> u32 {
    let hr = e.code().0 as u32;
    if hr >> 16 == 0x8007 {
        hr & 0xFFFF
    } else {
        hr
    }
}

fn until_nul(units: &[u16]) -> &[u16] {
    let n = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    units.get(..n).unwrap_or_default()
}

/// One NTFS name component: not empty, not "." or "..", no NUL, control character or \ / : * ? " < > |.
fn is_plain_name(units: &[u16]) -> bool {
    let bad = |u: u16| u < 0x20 || br#"\/:*?"<>|"#.iter().any(|&b| u == u16::from(b));
    let dot = u16::from(b'.');
    !units.is_empty() && units != [dot] && units != [dot, dot] && !units.iter().any(|&u| bad(u))
}

/// UTF-16 (lone surrogates allowed) to WTF-8.
fn utf16_to_wtf8(units: &[u16], out: &mut Vec<u8>) {
    for r in char::decode_utf16(units.iter().copied()) {
        match r {
            Ok(c) => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            Err(e) => {
                let u = e.unpaired_surrogate();
                out.extend_from_slice(&[
                    0xE0 | (u >> 12) as u8,
                    0x80 | ((u >> 6) & 0x3F) as u8,
                    0x80 | (u & 0x3F) as u8,
                ]);
            }
        }
    }
}

/// WTF-8 to UTF-16; false on bytes that are not WTF-8 (`out` then holds a prefix).
fn wtf8_to_utf16(bytes: &[u8], out: &mut Vec<u16>) -> bool {
    let mut it = bytes.iter().copied();
    while let Some(b0) = it.next() {
        let (len, init) = match b0 {
            0x00..=0x7F => (0, u32::from(b0)),
            0xC2..=0xDF => (1, u32::from(b0 & 0x1F)),
            0xE0..=0xEF => (2, u32::from(b0 & 0x0F)),
            0xF0..=0xF4 => (3, u32::from(b0 & 0x07)),
            _ => return false,
        };
        let mut cp = init;
        for _ in 0..len {
            match it.next() {
                Some(b) if b & 0xC0 == 0x80 => cp = (cp << 6) | u32::from(b & 0x3F),
                _ => return false,
            }
        }
        let min = [0, 0x80, 0x800, 0x1_0000][len];
        if cp < min || cp > 0x10_FFFF {
            return false; // overlong or out of range
        }
        if cp >= 0x1_0000 {
            let v = cp - 0x1_0000;
            out.push(0xD800 | (v >> 10) as u16);
            out.push(0xDC00 | (v & 0x3FF) as u16);
        } else {
            out.push(cp as u16); // a lone surrogate (ED A0..BF xx) stays one unit: that is WTF-8
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;

    use super::*;
    use crate::testkit::TempRoot;

    fn wide_path(p: &std::path::Path) -> WidePath {
        WidePath::new(&p.as_os_str().encode_wide().collect::<Vec<u16>>()).unwrap()
    }

    #[test]
    fn stat_present() {
        let root = TempRoot::new("stat_present");
        root.file("a.txt", b"hello");
        let dir = wide_path(root.path());
        let got = stat_one(&dir, b"a.txt").unwrap().expect("a.txt is there");
        assert_eq!(&*got.name, b"a.txt");
        assert_eq!((got.e.name_off, usize::from(got.e.name_len)), (0, 5));
        assert_eq!(got.e.size, 5);
        assert_eq!(got.e.attrs & FILE_ATTRIBUTE_DIRECTORY.0, 0);
        assert!(got.e.mtime > 0 && got.e.ctime > 0);
        let upper = stat_one(&dir, b"A.TXT").unwrap().expect("names are case-insensitive");
        assert_eq!(&*upper.name, b"a.txt", "the name as stored");
    }

    #[test]
    fn stat_short_name_finds_the_long_name() {
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;
        let root = TempRoot::new("stat_short");
        let long = root.file("longfilename.txt", b"long");
        let wide: Vec<u16> = long.as_os_str().encode_wide().chain([0]).collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: `wide` is NUL-terminated and lives for the call; `buf` is writable for its whole length.
        let n = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut buf)) } as usize;
        assert!(n > 0 && n < buf.len(), "GetShortPathNameW: {}", std::io::Error::last_os_error());
        let short_path = String::from_utf16(&buf[..n]).unwrap();
        let short = short_path.rsplit('\\').next().unwrap();
        if short == "longfilename.txt" {
            eprintln!("no 8.3 names on this volume: the short-name match is not exercised");
        }
        let dir = wide_path(root.path());
        let got = stat_one(&dir, short.as_bytes()).unwrap().expect("the short name names the file");
        assert_eq!(&*got.name, b"longfilename.txt", "{short} returns the stored long name");
        assert_eq!(got.e.size, 4);
    }

    #[test]
    fn stat_absent() {
        let root = TempRoot::new("stat_absent");
        let dir = wide_path(root.path());
        assert!(stat_one(&dir, b"nothing.txt").unwrap().is_none());
        let missing = wide_path(&root.path().join("no-such-dir"));
        assert!(stat_one(&missing, b"x").unwrap().is_none(), "a missing folder is no such name");
    }

    #[test]
    fn stat_directory() {
        let root = TempRoot::new("stat_directory");
        root.dir("sub");
        let got = stat_one(&wide_path(root.path()), b"sub").unwrap().expect("sub is there");
        assert_ne!(got.e.attrs & FILE_ATTRIBUTE_DIRECTORY.0, 0);
        assert_eq!(got.e.size, 0);
    }

    #[test]
    fn stat_long_name_beyond_260() {
        let root = TempRoot::new("stat_long");
        let seg = "d".repeat(50);
        let rel = format!("{seg}/{seg}/{seg}/{seg}/{seg}/{seg}");
        root.file(&format!("{rel}/deep.bin"), &[1, 2, 3]);
        let dir = wide_path(&root.at(&rel));
        assert!(dir.units().len() > 260);
        let got = stat_one(&dir, b"deep.bin").unwrap().expect("found beyond MAX_PATH");
        assert_eq!(got.e.size, 3);
    }

    #[test]
    fn stat_wildcard_is_none() {
        let root = TempRoot::new("stat_wildcard");
        root.file("a.txt", b"x");
        let dir = wide_path(root.path());
        for name in [&b"*.txt"[..], b"a?txt", b"*", b"", b".", b"..", b"sub\\a.txt", b"a.txt:s", b"a|b", b"\xff"] {
            assert!(stat_one(&dir, name).unwrap().is_none(), "{:?}", String::from_utf8_lossy(name));
        }
    }

    #[test]
    fn stat_lone_surrogate_name_round_trips() {
        let root = TempRoot::new("stat_wtf8");
        let units = [u16::from(b'x'), 0xD800, u16::from(b'y')];
        root.file_wide(&units, b"lone");
        let mut wtf8 = Vec::new();
        utf16_to_wtf8(&units, &mut wtf8);
        assert_eq!(wtf8, [b'x', 0xED, 0xA0, 0x80, b'y']);
        let got = stat_one(&wide_path(root.path()), &wtf8).unwrap().expect("found by its WTF-8 name");
        assert_eq!(&*got.name, &wtf8[..]);
        assert_eq!(got.e.size, 4);
    }

    #[test]
    fn wtf8_round_trip_and_refusals() {
        let samples: [&[u16]; 4] = [&[0x61, 0x62], &[0xD83D, 0xDE00], &[0xDC00, 0x41, 0xD800], &[0x00E9, 0x4E2D]];
        for s in samples {
            let mut b = Vec::new();
            utf16_to_wtf8(s, &mut b);
            let mut back = Vec::new();
            assert!(wtf8_to_utf16(&b, &mut back));
            assert_eq!(back, s);
        }
        for bad in [&[0xC0u8, 0x80][..], &[0xE0, 0x80, 0x80], &[0xF5, 0x80, 0x80, 0x80], &[0xE2, 0x82], &[0x80]] {
            assert!(!wtf8_to_utf16(bad, &mut Vec::new()), "{bad:?}");
        }
    }

    #[test]
    fn find_errors_map_as_the_enumerator_does() {
        assert_eq!(map_find_error(ERROR_ACCESS_DENIED.0, 0), EnumError::AccessDenied);
        assert_eq!(map_find_error(ERROR_DIRECTORY.0, 0), EnumError::NotFound);
        assert_eq!(map_find_error(ERROR_BAD_NET_NAME.0, 7), EnumError::Offline { waited_ms: 7 });
        assert_eq!(map_find_error(ERROR_FILENAME_EXCED_RANGE.0, 0), EnumError::TooLong);
        assert_eq!(map_find_error(ERROR_NOT_SUPPORTED.0, 0), EnumError::Unsupported);
        assert_eq!(map_find_error(5000, 0), EnumError::Win32(5000));
    }
}
