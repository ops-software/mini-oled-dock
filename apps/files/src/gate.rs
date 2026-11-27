//! The real-data gate (P3-files-app U2, F3.1). Until the FS-3 rows a to c are recorded, the window changes files only
//! inside the test and live roots under %TEMP% (P1-files-engine's TempRoot and LiveRoot): every item and destination
//! must lie in %TEMP%\mod-files-live-* or %TEMP%\mod-files-test-*, and no component from that root down to the item
//! may be a reparse point. The orchestrator flips REAL_DATA_MUTATIONS in its own reviewed commit.

use windows::core::PCWSTR;
use windows::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};
use windows::Win32::Storage::FileSystem::{
    GetFileAttributesW, GetLongPathNameW, FILE_ATTRIBUTE_REPARSE_POINT, INVALID_FILE_ATTRIBUTES,
};

use crate::listing::WidePath;

/// False until FS-3 rows a to c are recorded and their constants set (SPEC-files 8.1).
pub const REAL_DATA_MUTATIONS: bool = false;

/// The roots' name prefixes (P1-files-engine D2).
const ROOT_PREFIXES: [&str; 2] = ["mod-files-live-", "mod-files-test-"];

const BS: u16 = b'\\' as u16;

/// The system adapter of may_mutate_with: attributes from GetFileAttributesW, long forms from GetLongPathNameW.
pub fn may_mutate(paths: &[&WidePath]) -> bool {
    if REAL_DATA_MUTATIONS {
        return !paths.is_empty();
    }
    match WidePath::parse(&std::env::temp_dir().to_string_lossy()) {
        Ok(temp) => may_mutate_with(paths, &temp, &attrs_of, &long_of),
        Err(_) => false,
    }
}

/// The pure core. TMP may be an 8.3 form, so `temp` is compared in its long form too. Every path, in its long form,
/// must lie under `temp` in a root named mod-files-live-* or
/// mod-files-test-* (compared ordinally, ignoring case), and that root and every existing component below it down to
/// the path must not be a reparse point. A component that does not exist yet is not checked; its existing parent is.
/// An empty list, a path without a long form and a path outside the roots refuse the whole request.
pub fn may_mutate_with(
    paths: &[&WidePath],
    temp: &WidePath,
    attrs: &dyn Fn(&WidePath) -> Option<u32>,
    long: &dyn Fn(&WidePath) -> Option<WidePath>,
) -> bool {
    let Some(temp) = long(temp) else { return false };
    !paths.is_empty() && paths.iter().all(|p| long(p).is_some_and(|q| inside_a_root(&q, &temp, attrs)))
}

fn same_ci(a: &[u16], b: &[u16]) -> bool {
    // SAFETY: both slices are valid for their lengths; the call only reads them.
    a.len() == b.len() && unsafe { CompareStringOrdinal(a, b, true) } == CSTR_EQUAL
}

fn starts_ci(s: &[u16], prefix: &[u16]) -> bool {
    s.get(..prefix.len()).is_some_and(|head| same_ci(head, prefix))
}

fn inside_a_root(q: &WidePath, temp: &WidePath, attrs: &dyn Fn(&WidePath) -> Option<u32>) -> bool {
    let (units, t) = (q.units(), temp.units());
    if !starts_ci(units, t) {
        return false;
    }
    // After TEMP: one separator (already in TEMP when it is a drive root), then the root component.
    let rest = match units.get(t.len()..) {
        Some(r) if t.last() == Some(&BS) => r,
        Some([sep, r @ ..]) if *sep == BS => r,
        _ => return false,
    };
    let root_len = rest.iter().position(|&u| u == BS).unwrap_or(rest.len());
    let root = rest.get(..root_len).unwrap_or_default();
    let named = ROOT_PREFIXES.iter().any(|p| {
        let p: Vec<u16> = p.encode_utf16().collect();
        root.len() > p.len() && starts_ci(root, &p)
    });
    if !named {
        return false;
    }
    // The root and every component below it, down to the item: no reparse point among those that exist.
    let first = units.len() - rest.len() + root_len;
    let mut ends: Vec<usize> =
        rest.iter().enumerate().filter(|(_, &u)| u == BS).map(|(i, _)| units.len() - rest.len() + i).collect();
    ends.push(units.len());
    ends.into_iter().filter(|&e| e >= first).all(|e| {
        let prefix = units.get(..e).and_then(|u| WidePath::new(u).ok());
        match prefix {
            Some(p) => attrs(&p).is_none_or(|a| a & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0),
            None => false,
        }
    })
}

/// GetFileAttributesW; None when the path does not exist (or cannot be read).
fn attrs_of(p: &WidePath) -> Option<u32> {
    let raw = p.raw();
    // SAFETY: raw is NUL-terminated and lives for the call.
    let a = unsafe { GetFileAttributesW(PCWSTR(raw.as_ptr())) };
    (a != INVALID_FILE_ATTRIBUTES).then_some(a)
}

/// The long form: GetLongPathNameW of the longest existing prefix, the rest appended as written.
fn long_of(p: &WidePath) -> Option<WidePath> {
    let mut base = p.clone();
    let mut tail: Vec<Vec<u16>> = Vec::new();
    while attrs_of(&base).is_none() {
        tail.push(base.file_name()?.to_vec());
        base = base.parent()?;
    }
    let mut long = long_path(&base)?;
    for name in tail.iter().rev() {
        long = long.join(name).ok()?;
    }
    Some(long)
}

/// GetLongPathNameW of a path that exists: every 8.3 alias in it replaced by the long name; None when it fails.
pub(crate) fn long_path(p: &WidePath) -> Option<WidePath> {
    let raw = p.raw();
    let mut buf = vec![0u16; 1024];
    // SAFETY: raw is NUL-terminated; buf is writable for its length.
    let mut n = unsafe { GetLongPathNameW(PCWSTR(raw.as_ptr()), Some(&mut buf)) } as usize;
    if n > buf.len() {
        buf = vec![0u16; n];
        // SAFETY: as above, with the size the first call asked for.
        n = unsafe { GetLongPathNameW(PCWSTR(raw.as_ptr()), Some(&mut buf)) } as usize;
    }
    if n == 0 || n > buf.len() {
        return None;
    }
    WidePath::new(buf.get(..n)?).ok()
}
